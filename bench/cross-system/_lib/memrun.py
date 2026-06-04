#!/usr/bin/env python3
"""Run a command under a wall-clock + peak-RSS monitor with a hard
memory cap, for the cross-system bench.

Why this exists
---------------
The cross-system harness benches gqlite (a Rust binary) alongside Kuzu
and Grafeo (Python wheels). Each runner already measures *time*. What it
did NOT do is measure memory authoritatively or bound it: the in-runner
psutil sampling (a) requires psutil to be installed, (b) samples once per
params-row (so a transient mid-query spike is missed), and (c) cannot
abort a runaway query. On a shared bench server an unbounded shortest-path
or cartesian blow-up can eat all RAM and take the box down.

`memrun.py` wraps ONE runner invocation. It:
  * launches the command in its own session (process group) so the whole
    descendant tree can be sampled and killed as a unit;
  * samples the process group's total resident set (RSS) from `/proc`
    every `--interval` seconds — pure stdlib, no psutil, Linux-only
    (the bench server is Linux);
  * tracks the peak;
  * if the peak crosses `--limit-bytes` (default 10 GiB) it SIGKILLs the
    whole group and reports a memory error (exit code 137);
  * writes a machine-readable `key=value` summary to `--peak-out` so the
    orchestrator can aggregate per-(system, IC) memory + time without
    scraping human text.

The monitor reads RSS from `/proc/<pid>/statm` (resident pages) and the
process group id from `/proc/<pid>/stat`. RSS double-counts shared pages
across processes in the group, which is the conservative direction for a
safety cap (we'd rather trip slightly early than OOM the host).

Usage
-----
    python memrun.py --peak-out <file> [--label L] [--limit-bytes N]
                     [--limit-gb G] [--interval S] -- <cmd> [args...]

Everything after `--` is the command, run with the current environment
(so callers can prefix env vars, e.g. `GQLITE_DISABLE_INDEX_FOLD=1`,
before `python memrun.py`). stdout/stderr of the child are inherited.

Exit code: the child's exit code on a clean run; 137 if the child was
killed for exceeding the memory cap; 1 on a memrun-internal error.
"""

from __future__ import annotations

import argparse
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

PAGE_SIZE = os.sysconf("SC_PAGE_SIZE") if hasattr(os, "sysconf") else 4096
DEFAULT_LIMIT_BYTES = 10 * 1024 * 1024 * 1024  # 10 GiB
OOM_EXIT_CODE = 137  # 128 + SIGKILL(9), the conventional "killed" code
TIMEOUT_EXIT_CODE = 124  # GNU `timeout`'s convention


def _read_pgrp(pid: int) -> int | None:
    """Process-group id of `pid` from /proc/<pid>/stat. `comm` (field 2)
    can contain spaces and parentheses, so split on the LAST ')' and
    index the remaining whitespace-separated fields: state(0) ppid(1)
    pgrp(2)."""
    try:
        with open(f"/proc/{pid}/stat", "rb") as f:
            data = f.read()
    except (FileNotFoundError, ProcessLookupError, PermissionError):
        return None
    rparen = data.rfind(b")")
    if rparen == -1:
        return None
    rest = data[rparen + 2:].split()
    if len(rest) < 3:
        return None
    try:
        return int(rest[2])
    except ValueError:
        return None


def _read_rss_bytes(pid: int) -> int:
    """Resident set size of `pid` in bytes from /proc/<pid>/statm
    (field 2 = resident pages). 0 if the process vanished."""
    try:
        with open(f"/proc/{pid}/statm", "rb") as f:
            fields = f.read().split()
    except (FileNotFoundError, ProcessLookupError, PermissionError):
        return 0
    if len(fields) < 2:
        return 0
    try:
        return int(fields[1]) * PAGE_SIZE
    except ValueError:
        return 0


def _group_rss_bytes(pgid: int) -> int:
    """Sum RSS over every process whose process-group id == pgid."""
    total = 0
    try:
        entries = os.listdir("/proc")
    except OSError:
        return 0
    for name in entries:
        if not name.isdigit():
            continue
        pid = int(name)
        if _read_pgrp(pid) == pgid:
            total += _read_rss_bytes(pid)
    return total


def _proc_available() -> bool:
    return os.path.isdir("/proc/self")


def _write_peak_out(path: Path, fields: dict[str, object]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as f:
        for k, v in fields.items():
            f.write(f"{k}={v}\n")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--peak-out", type=Path, required=True,
                    help="path to write the key=value memory/time summary")
    ap.add_argument("--label", default="",
                    help="free-form label echoed into the summary")
    ap.add_argument("--limit-bytes", type=int, default=None,
                    help=f"hard RSS cap in bytes (default {DEFAULT_LIMIT_BYTES})")
    ap.add_argument("--limit-gb", type=float, default=None,
                    help="hard RSS cap in GiB (overridden by --limit-bytes)")
    ap.add_argument("--interval", type=float, default=0.05,
                    help="sampling interval in seconds (default 0.05)")
    ap.add_argument("--timeout-s", type=float, default=None,
                    help="hard wall-clock cap in seconds for the whole runner "
                         "invocation; on expiry the process group is SIGKILLed "
                         "and the run recorded as a timeout (default: no cap)")
    ap.add_argument("cmd", nargs=argparse.REMAINDER,
                    help="-- <command> [args...]")
    args = ap.parse_args()

    cmd = args.cmd
    if cmd and cmd[0] == "--":
        cmd = cmd[1:]
    if not cmd:
        sys.stderr.write("memrun: no command given (expected `-- <cmd> ...`)\n")
        return 1

    if args.limit_bytes is not None:
        limit = args.limit_bytes
    elif args.limit_gb is not None:
        limit = int(args.limit_gb * 1024 * 1024 * 1024)
    else:
        limit = DEFAULT_LIMIT_BYTES

    label = args.label or " ".join(cmd[:2])
    limit_mib = limit / (1024 * 1024)

    if not _proc_available():
        # Non-Linux / no procfs: run unmonitored rather than fail. The
        # bench server is Linux, so this is a portability courtesy.
        sys.stderr.write(
            f"memrun[{label}]: /proc unavailable; running WITHOUT memory "
            f"monitoring or cap.\n"
        )
        t0 = time.perf_counter()
        rc = subprocess.call(cmd)
        elapsed = time.perf_counter() - t0
        _write_peak_out(args.peak_out, {
            "label": label,
            "status": "ok" if rc == 0 else "runner_error",
            "exit_code": rc,
            "peak_rss_bytes": -1,
            "peak_rss_mib": -1,
            "limit_bytes": limit,
            "limit_mib": f"{limit_mib:.1f}",
            "elapsed_s": f"{elapsed:.3f}",
            "monitored": 0,
        })
        return rc

    sys.stderr.write(
        f"memrun[{label}]: cap {limit_mib:.0f} MiB, "
        f"sample every {args.interval * 1000:.0f} ms\n"
    )

    # start_new_session=True => the child becomes a session+group leader,
    # so its descendants share the group and we can sample/kill the tree.
    t0 = time.perf_counter()
    try:
        proc = subprocess.Popen(cmd, start_new_session=True)
    except FileNotFoundError as e:
        sys.stderr.write(f"memrun[{label}]: cannot exec {cmd[0]!r}: {e}\n")
        _write_peak_out(args.peak_out, {
            "label": label, "status": "runner_error", "exit_code": 127,
            "peak_rss_bytes": -1, "peak_rss_mib": -1, "limit_bytes": limit,
            "limit_mib": f"{limit_mib:.1f}", "elapsed_s": "0.000", "monitored": 1,
        })
        return 127

    pgid = proc.pid  # session/group leader pid == pgid
    peak = 0
    oom = False
    timed_out = False

    while True:
        rc = proc.poll()
        rss = _group_rss_bytes(pgid)
        if rss > peak:
            peak = rss
        if peak > limit:
            oom = True
            sys.stderr.write(
                f"memrun[{label}]: MEMORY LIMIT EXCEEDED — peak "
                f"{peak / (1024 * 1024):.1f} MiB > cap {limit_mib:.0f} MiB; "
                f"killing process group {pgid}.\n"
            )
            try:
                os.killpg(pgid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            break
        if args.timeout_s is not None and (time.perf_counter() - t0) > args.timeout_s:
            timed_out = True
            sys.stderr.write(
                f"memrun[{label}]: TIME LIMIT EXCEEDED — ran "
                f"{time.perf_counter() - t0:.0f}s > cap {args.timeout_s:.0f}s; "
                f"killing process group {pgid}.\n"
            )
            try:
                os.killpg(pgid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            break
        if rc is not None:
            break
        time.sleep(args.interval)

    # Reap (and on OOM, ensure the whole group is gone).
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(pgid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        proc.wait()
    # Sweep any lingering group members (orphaned grandchildren).
    if _group_rss_bytes(pgid) > 0:
        try:
            os.killpg(pgid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    elapsed = time.perf_counter() - t0
    child_rc = proc.returncode if proc.returncode is not None else -1

    if oom:
        status = "memory_error"
        exit_code = OOM_EXIT_CODE
    elif timed_out:
        status = "timeout"
        exit_code = TIMEOUT_EXIT_CODE
    elif child_rc == 0:
        status = "ok"
        exit_code = 0
    else:
        status = "runner_error"
        exit_code = child_rc if child_rc > 0 else 1

    peak_mib = peak / (1024 * 1024)
    _write_peak_out(args.peak_out, {
        "label": label,
        "status": status,
        "exit_code": exit_code,
        "peak_rss_bytes": peak,
        "peak_rss_mib": f"{peak_mib:.1f}",
        "limit_bytes": limit,
        "limit_mib": f"{limit_mib:.1f}",
        "timeout_s": args.timeout_s if args.timeout_s is not None else 0,
        "elapsed_s": f"{elapsed:.3f}",
        "monitored": 1,
    })
    sys.stderr.write(
        f"memrun[{label}]: status={status} peak={peak_mib:.1f} MiB "
        f"elapsed={elapsed:.2f}s exit={exit_code}\n"
    )
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
