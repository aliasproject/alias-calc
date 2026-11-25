#!/usr/bin/env python3
"""Measure one GUI calculator's launch on Hyprland.

Each run starts the command, then times how long Hyprland takes to report the
window opened (`openwindow` on its event socket, sent once the app's first
buffer maps the toplevel). After the window settles, it reads PSS, RSS and the
thread count for the process tree, then counts context switches (wakeups) over
an idle period. It prints the medians as one JSON line.

Usage: calcbench.py [--runs N] <name> <window-class-substring> -- cmd args...
       calcbench.py --table < results.jsonl

The windows open on the current workspace, so leave the machine alone while it
runs.
"""
import argparse
import json
import os
import signal
import socket
import statistics
import subprocess
import sys
import time

SETTLE = 2.0  # seconds after map before reading memory
IDLE = 5.0  # seconds of idle to count wakeups over
MAP_TIMEOUT = 10.0


def event_socket():
    sig = os.environ.get("HYPRLAND_INSTANCE_SIGNATURE")
    if not sig:
        sys.exit("calcbench: needs a Hyprland session (HYPRLAND_INSTANCE_SIGNATURE is unset)")
    return f"{os.environ['XDG_RUNTIME_DIR']}/hypr/{sig}/.socket2.sock"


def tree(pid):
    """The process and all its descendants."""
    pids, todo = [], [pid]
    while todo:
        p = todo.pop()
        pids.append(p)
        try:
            for t in os.listdir(f"/proc/{p}/task"):
                with open(f"/proc/{p}/task/{t}/children") as f:
                    todo += [int(c) for c in f.read().split()]
        except OSError:
            pass
    return pids


def memory(pids):
    pss = rss = threads = 0
    for p in pids:
        try:
            with open(f"/proc/{p}/smaps_rollup") as f:
                for line in f:
                    key, value = line.split(":", 1)
                    if key == "Pss":
                        pss += int(value.split()[0])
                    elif key == "Rss":
                        rss += int(value.split()[0])
            threads += len(os.listdir(f"/proc/{p}/task"))
        except OSError:
            pass
    return pss, rss, threads


def switches(pids):
    total = 0
    for p in pids:
        try:
            for t in os.listdir(f"/proc/{p}/task"):
                with open(f"/proc/{p}/task/{t}/status") as f:
                    for line in f:
                        if line.startswith(("voluntary_ctxt", "nonvoluntary_ctxt")):
                            total += int(line.split()[1])
        except OSError:
            pass
    return total


def wait_for_window(sock, wclass):
    deadline = time.perf_counter() + MAP_TIMEOUT
    buf = b""
    while True:
        sock.settimeout(max(0.01, deadline - time.perf_counter()))
        chunk = sock.recv(65536)
        if not chunk:
            raise ConnectionError("Hyprland closed the event socket")
        buf += chunk
        *lines, buf = buf.split(b"\n")
        for line in lines:
            if line.startswith(b"openwindow>>") and wclass in line.decode(errors="replace").lower():
                return time.perf_counter()


def run_once(cmd, wclass, path):
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(path)
    start = time.perf_counter()
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                            start_new_session=True)
    try:
        mapped = wait_for_window(sock, wclass)
        sock.close()
        time.sleep(SETTLE)
        pids = tree(proc.pid)
        pss, rss, threads = memory(pids)
        before = switches(pids)
        time.sleep(IDLE)
        wakeups = (switches(tree(proc.pid)) - before) / IDLE
    finally:
        sock.close()
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        proc.wait()
        time.sleep(0.5)
    return dict(map_ms=(mapped - start) * 1000, pss_kb=pss, rss_kb=rss, threads=threads,
                wakeups_s=wakeups)


def table():
    """Print calcbench JSON lines from stdin as one table."""
    rows = [json.loads(line) for line in sys.stdin if line.strip()]
    cols = [("map_ms", "map ms"), ("map_ms_min", "min ms"), ("pss_kb", "PSS MiB"),
            ("rss_kb", "RSS MiB"), ("threads", "threads"), ("wakeups_s", "wakeups/s")]
    print(" " * 18 + "".join(f"{label:>11}" for _, label in cols))
    for row in rows:
        cells = [row[k] / 1024 if k.endswith("_kb") else row[k] for k, _ in cols]
        print(f"{row['name']:18}" + "".join(f"{v:>11.1f}" for v in cells))
    if rows:
        print(f"median of {rows[0]['runs']} runs each")


def main():
    argv = sys.argv[1:]
    if argv == ["--table"]:
        return table()
    if "--" not in argv:
        sys.exit(__doc__)
    split = argv.index("--")
    parser = argparse.ArgumentParser()
    parser.add_argument("--runs", type=int, default=10)
    parser.add_argument("name")
    parser.add_argument("wclass")
    args = parser.parse_args(argv[:split])
    cmd = argv[split + 1:]
    path = event_socket()

    try:
        results = [run_once(cmd, args.wclass.lower(), path) for _ in range(args.runs)]
    except (TimeoutError, socket.timeout):
        sys.exit(f"calcbench: {args.name}: no window within {MAP_TIMEOUT:.0f} s")

    median = {k: round(statistics.median(r[k] for r in results), 1) for k in results[0]}
    median["map_ms_min"] = round(min(r["map_ms"] for r in results), 1)
    print(json.dumps({"name": args.name, "runs": args.runs, **median}))


if __name__ == "__main__":
    main()
