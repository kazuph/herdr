"""Compare real PTY delivery in an isolated debug server before/after the fix.

The two shell readers use the normal agent-report API with the same codex label;
no model calls, installed server changes, or production database writes occur.
"""

import argparse
import json
import os
import select
import socket
import sqlite3
import subprocess
import tempfile
import time
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument("binary")
p.add_argument("--expect", choices=["broken", "fixed"], required=True)
p.add_argument("--output", required=True)
a = p.parse_args()
binary = str(Path(a.binary).resolve())
base = Path(tempfile.mkdtemp(prefix="hc-", dir="/tmp"))
env = {k: v for k, v in os.environ.items() if not k.startswith("HERDR_")}
env.update(
    HOME=str(base / "home"),
    XDG_CONFIG_HOME=str(base / "config"),
    XDG_STATE_HOME=str(base / "state"),
    XDG_RUNTIME_DIR=str(base / "runtime"),
    SHELL="/bin/sh",
)
for key in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR"]:
    Path(env[key]).mkdir(parents=True)
session = "caller-reproduction"
namespace = (
    "herdr-dev"
    if Path(binary).stem in ["herdr", "herdr-dev", "herdr-debug"]
    else Path(binary).stem
)
data = base / "config" / namespace / "sessions" / session
sock = data / "herdr.sock"


def cli(*args):
    return subprocess.run(
        [binary, "--session", session, *args],
        env=env,
        text=True,
        capture_output=True,
        timeout=30,
    )


def api(method, params):
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(30)
        s.connect(str(sock))
        s.sendall(
            (
                json.dumps(dict(id="caller-test", method=method, params=params)) + "\n"
            ).encode()
        )
        r = b""
        while not r.endswith(b"\n"):
            chunk = s.recv(65536)
            if not chunk:
                raise RuntimeError("isolated server closed the API connection")
            r += chunk
    response = json.loads(r)
    if "error" in response:
        raise RuntimeError(response)
    return response["result"]


def until(f):
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        r = f()
        if r:
            return r
        select.select([], [], [], 0.05)
    raise TimeoutError("isolated reproduction timed out")


log = (base / "server.log").open("w")
server = subprocess.Popen(
    [binary, "--session", session, "server"], env=env, stdout=log, stderr=log
)
try:
    until(sock.exists)
    panes = [
        api("workspace.create", dict(cwd=str(base), focus=False, label=f"caller-{i}"))[
            "root_pane"
        ]["pane_id"]
        for i in range(2)
    ]
    first, second = panes
    api("agent.rename", dict(target=first, name="codex"))

    def report(pane):
        return api(
            "pane.report_agent",
            dict(pane_id=pane, source="codex", agent="codex", state="working"),
        )

    report(first)
    for pane in panes:
        capture = base / f"{pane}.input"
        command = (
            "exec /bin/sh -c 'while IFS= read -r line; "
            f' do printf "%s\\n" "$line" >> {capture}; done' + "'"
        )
        api("pane.send_input", dict(pane_id=pane, text=command, keys=["Enter"]))
    gate = base / "gate"
    os.mkfifo(gate)
    started = cli(
        "run",
        "--caller",
        first,
        "--label",
        "same-name",
        "--",
        "/bin/sh",
        "-c",
        f"read line < {gate}",
    )
    assert started.returncode == 0, started.stderr + started.stdout
    job = json.loads(started.stdout)["job"]
    report(second)
    with gate.open("w") as f:
        f.write("finish\n")
    db = data / "herdr.db"

    def rows():
        with sqlite3.connect(db) as c:
            return c.execute(
                "SELECT d.id,ta.name,d.status,d.delivered_at,d.body FROM dispatches d "
                "JOIN actors ta ON ta.id=d.to_actor "
                "WHERE d.kind='message' AND d.room='herdr-jobs'"
            ).fetchall()

    notices = until(rows)
    if a.expect == "fixed":
        notices = until(
            lambda: (r if any(x[2] == "delivered" for x in r) else None)
            if (r := rows())
            else None
        )
        until(
            lambda: (base / f"{first}.input").exists()
            and job in (base / f"{first}.input").read_text()
        )
    captures = {
        pane: (
            (base / f"{pane}.input").read_text()
            if (base / f"{pane}.input").exists()
            else ""
        )
        for pane in panes
    }
    result = dict(
        base=str(base),
        binary=binary,
        panes=panes,
        job=job,
        notices=notices,
        captures=captures,
    )
    if a.expect == "fixed":
        assert all(r[1] == first and r[2] == "delivered" for r in notices), result
        assert job in captures[first] and job not in captures[second], result
    else:
        assert job not in captures[first] and (
            any(r[2] == "queued" for r in notices) or job in captures[second]
        ), result
    if a.expect == "fixed":
        rejected = cli(
            "send",
            "codex",
            "ambiguous-send-proof",
            "--room",
            "caller-regression",
            "--from",
            "tester",
        )
        assert rejected.returncode != 0, rejected.stdout
        with sqlite3.connect(db) as c:
            failure = c.execute(
                "SELECT status,delivery_error FROM dispatches "
                "WHERE body='ambiguous-send-proof'"
            ).fetchone()
        assert failure and failure[0] == "failed" and failure[1], failure
        assert all("ambiguous-send-proof" not in text for text in captures.values())
        accepted = cli(
            "send",
            first,
            "exact-send-proof",
            "--room",
            "caller-regression",
            "--from",
            "tester",
        )
        assert accepted.returncode == 0, accepted.stderr
        until(lambda: "exact-send-proof" in (base / f"{first}.input").read_text())
        full = cli(
            "run",
            "--caller",
            first,
            "--label",
            "full-proof",
            "--completion",
            "full",
            "--",
            "/usr/bin/printf",
            "full-log-proof\n",
        )
        assert full.returncode == 0, full.stderr
        full_job = json.loads(full.stdout)["job"]
        until(lambda: "full-log-proof" in (base / f"{first}.input").read_text())
        pane_run = cli(
            "run",
            "--caller",
            first,
            "--label",
            "pane-proof",
            "--pane",
            "--",
            "/usr/bin/true",
        )
        assert pane_run.returncode == 0, pane_run.stderr + pane_run.stdout
        until(lambda: "label=pane-proof pane=" in (base / f"{first}.input").read_text())
        missing_gate = base / "missing-gate"
        os.mkfifo(missing_gate)
        missing = cli(
            "run",
            "--caller",
            second,
            "--label",
            "missing-proof",
            "--",
            "/bin/sh",
            "-c",
            f"read line < {missing_gate}",
        )
        assert missing.returncode == 0, missing.stderr
        missing_job = json.loads(missing.stdout)["job"]
        api("pane.close", dict(pane_id=second))
        with missing_gate.open("w") as f:
            f.write("finish\n")

        def failed_notice():
            with sqlite3.connect(db) as c:
                return c.execute(
                    "SELECT d.status,d.delivery_error,d.body,ta.name FROM dispatches d "
                    "JOIN actors ta ON ta.id=d.to_actor "
                    "WHERE d.kind='message' AND d.body LIKE ?",
                    (f"%job={missing_job}%",),
                ).fetchone()

        failed = until(failed_notice)
        assert failed[0] == "failed" and failed[1] and failed[3] == second, failed
        assert missing_job not in (base / f"{first}.input").read_text()
        legacy = cli(
            "send",
            "codex",
            "legacy-unique-completion-proof",
            "--room",
            "herdr-jobs",
            "--from",
            "herdr-run",
        )
        assert legacy.returncode != 0, legacy.stdout
        with sqlite3.connect(db) as c:
            legacy_record = c.execute(
                "SELECT status,delivery_error FROM dispatches "
                "WHERE body='legacy-unique-completion-proof'"
            ).fetchone()
        assert legacy_record[0] == "failed" and legacy_record[1], legacy_record
        assert (
            "legacy-unique-completion-proof"
            not in (base / f"{first}.input").read_text()
        )
        result["failure_cases"] = dict(
            ambiguous_send=failure,
            missing_caller=failed,
            legacy_unique_completion=legacy_record,
        )
        result["additional_delivery"] = dict(
            exact_send="delivered",
            full_job=full_job,
            pane_mode=json.loads(pane_run.stdout),
        )
        result["final_capture"] = (base / f"{first}.input").read_text()
    Path(a.output).write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
finally:
    cli("server", "stop")
    server.wait(timeout=30)
    log.close()
