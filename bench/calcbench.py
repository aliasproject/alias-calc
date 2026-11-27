#!/usr/bin/env python3
"""Measure one GUI calculator's launch on Hyprland.

Each run starts the command, then times how long Hyprland takes to report the
window opened (`openwindow` on its event socket, sent once the app's first
buffer maps the toplevel). After the window settles, it reads PSS, RSS and the
thread count for the process tree, then counts context switches (wakeups) over
an idle period. It prints the medians as one JSON line. With --cold, it
drops the page cache before each run (sudo -n: ask for sudo first).

Usage: calcbench.py [--runs N] [--cold] <name> <window-class> -- cmd args...
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
COLD_MAP_TIMEOUT = 30.0  # a start that reads everything from disk


def event_socket():
    sig = os.environ.get("HYPRLAND_INSTANCE_SIGNATURE")
    if not sig:
        sys.exit("calcbench: needs a Hyprland session (HYPRLAND_INSTANCE_SIGNATURE is unset)")
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if not runtime:
        sys.exit("calcbench: XDG_RUNTIME_DIR is unset")
    return f"{runtime}/hypr/{sig}/.socket2.sock"


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
    """Context switches per thread, as {(pid, tid): count}. Voluntary ones
    (the thread slept and was woken) plus involuntary ones (preempted while
    running): both mean the thread was on a CPU."""
    counts = {}
    for p in pids:
        try:
            tids = os.listdir(f"/proc/{p}/task")
        except OSError:
            continue
        for t in tids:
            try:
                with open(f"/proc/{p}/task/{t}/status") as f:
                    counts[(p, t)] = sum(
                        int(line.split()[1])
                        for line in f
                        if line.startswith(("voluntary_ctxt", "nonvoluntary_ctxt"))
                    )
            except OSError:
                pass
    return counts


def wakeups(before, after):
    """Switches between two samples. A thread in both counts its difference;
    one that started in between counts all of its switches; one that ended
    counts none it made after the first sample (they can't be read)."""
    return sum(n - before.get(key, 0) for key, n in after.items())


def window_class(line):
    """The class in `openwindow>>ADDRESS,WORKSPACE,CLASS,TITLE`, lowercased."""
    payload = line.decode(errors="replace").removeprefix("openwindow>>")
    fields = payload.split(",", 3)
    return fields[2].lower() if len(fields) >= 3 else None


def wait_for_window(sock, wclass, proc):
    deadline = time.perf_counter() + MAP_TIMEOUT
    buf = b""
    while True:
        left = deadline - time.perf_counter()
        # Other windows' events keep arriving, so the deadline is checked
        # here, not only through the socket timeout.
        if left <= 0:
            raise TimeoutError
        if proc.poll() is not None:
            raise RuntimeError(f"exited with status {proc.returncode} before opening a window")
        sock.settimeout(min(left, 0.25))
        try:
            chunk = sock.recv(65536)
        except (TimeoutError, socket.timeout):
            continue
        if not chunk:
            raise ConnectionError("Hyprland closed the event socket")
        buf += chunk
        *lines, buf = buf.split(b"\n")
        for line in lines:
            if line.startswith(b"openwindow>>") and window_class(line) == wclass:
                return time.perf_counter()


def still_running(proc, when):
    if proc.poll() is not None:
        raise RuntimeError(f"exited with status {proc.returncode} {when}")


def run_once(cmd, wclass, path):
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(path)
    start = time.perf_counter()
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                            start_new_session=True)
    try:
        mapped = wait_for_window(sock, wclass, proc)
        sock.close()
        time.sleep(SETTLE)
        # A crashed app would otherwise read as zero memory and no wakeups.
        still_running(proc, "while settling")
        pids = tree(proc.pid)
        pss, rss, threads = memory(pids)
        before = switches(pids)
        time.sleep(IDLE)
        still_running(proc, "while idle")
        idle_wakeups = wakeups(before, switches(tree(proc.pid))) / IDLE
    finally:
        sock.close()
        stop(proc)
    return dict(map_ms=(mapped - start) * 1000, pss_kb=pss, rss_kb=rss, threads=threads,
                wakeups_s=idle_wakeups)


def drop_caches():
    """Empty the page cache, so the next start reads from disk. bench/run
    asked for sudo once, so this doesn't prompt."""
    subprocess.run(["sync"], check=True)
    subprocess.run(["sudo", "-n", "tee", "/proc/sys/vm/drop_caches"], input=b"3\n",
                   stdout=subprocess.DEVNULL, check=True)
    time.sleep(SETTLE)


def stop(proc):
    """SIGTERM the app's process group, then SIGKILL it if it lingers (a
    lingering GNOME Calculator would take over the next run's launch)."""
    for sig, wait in ((signal.SIGTERM, 3), (signal.SIGKILL, 3)):
        try:
            os.killpg(proc.pid, sig)
        except ProcessLookupError:
            break
        try:
            proc.wait(timeout=wait)
            break
        except subprocess.TimeoutExpired:
            continue
    # Helpers the app started may outlive it in its group.
    try:
        os.killpg(proc.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    time.sleep(0.5)


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
        cold = ", from a cold page cache" if any(r.get("cold") for r in rows) else ""
        print(f"median of {rows[0]['runs']} runs each{cold}")


def main():
    argv = sys.argv[1:]
    if argv == ["--table"]:
        return table()
    if "--" not in argv:
        sys.exit(__doc__)
    split = argv.index("--")
    parser = argparse.ArgumentParser()
    parser.add_argument("--runs", type=int, default=10, choices=range(1, 1001), metavar="N")
    parser.add_argument("--cold", action="store_true")
    parser.add_argument("name")
    parser.add_argument("wclass")
    args = parser.parse_args(argv[:split])
    cmd = argv[split + 1:]
    path = event_socket()
    global MAP_TIMEOUT
    if args.cold:
        MAP_TIMEOUT = COLD_MAP_TIMEOUT

    try:
        results = []
        for _ in range(args.runs):
            if args.cold:
                drop_caches()
            results.append(run_once(cmd, args.wclass.lower(), path))
    except (TimeoutError, socket.timeout):
        sys.exit(f"calcbench: {args.name}: no window within {MAP_TIMEOUT:.0f} s")
    except subprocess.CalledProcessError as err:
        why = "; --cold needs sudo, asked for first by bench/run" if err.cmd[0] == "sudo" else ""
        sys.exit(f"calcbench: {args.name}: couldn't drop the caches ({' '.join(err.cmd)}"
                 f" exited {err.returncode}){why}")
    except (OSError, RuntimeError) as err:
        sys.exit(f"calcbench: {args.name}: {err}")

    median = {k: round(statistics.median(r[k] for r in results), 1) for k in results[0]}
    median["map_ms_min"] = round(min(r["map_ms"] for r in results), 1)
    print(json.dumps({"name": args.name, "runs": args.runs, "cold": args.cold, **median}))


if __name__ == "__main__":
    main()
