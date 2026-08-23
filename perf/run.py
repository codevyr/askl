#!/usr/bin/env python3
"""askl query performance harness — measure cold per-query latency.

Runs each query in `queries.txt` against a **cache-off** staging askld, clearing
BOTH caches before every run (RAM SQL cache + DB ephemeral layers) via the
loopback `POST /admin/clear-cache` endpoint, so each measurement is cold. Writes
a ranked `REPORT.md`.

Prereq — a staging askld with the SQL cache disabled and a generous timeout:
    ASKL_SQL_CACHE_BYTES=0 ASKL_QUERY_TIMEOUT=120 \
      ASKL_DATABASE_URL=postgres://postgres:postgres@<compose-db>:5432/askl \
      ./target/release/askld serve --host 127.0.0.1 --port 8099 > /tmp/askld.log 2>&1
(release build — debug inflates the Rust-side latency).

W5 result-size characterisation (`--logfile`): the index crate logs, per SQL,
`result_rows=` / `result_bytes=` on its `select_*` / `find_*` info spans (default
`info` level, so no `RUST_LOG` needed).  Point `--logfile` at the askld log
(direct-process stdout redirect, or `docker compose logs -f <svc> > file`); the
harness snapshots the file offset before each query and reports the MAX rows and
bytes any single SQL returned for that query — the "how much does each query ship
to Rust" signal.

Usage:
    python3 run.py [--repeats N] [--base URL] [--logfile PATH]
"""
import argparse
import subprocess
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent


def post(base, path, body=b"", timeout=200):
    req = urllib.request.Request(base + path, data=body, method="POST",
                                 headers={"Content-Type": "text/plain"})
    return urllib.request.urlopen(req, timeout=timeout).read().decode()


def clear_cache(base, clear_cmd):
    """Drop caches before a cold run.  With `--clear-cmd` (e.g. a psql
    `DELETE FROM index.layers WHERE kind='ephemeral'` via `docker exec`), run
    that instead of the loopback endpoint — needed for the compose stack, whose
    `/admin/local/clear-cache` is loopback-gated and 404s from the host."""
    if clear_cmd:
        subprocess.run(clear_cmd, shell=True, check=False,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    else:
        post(base, "/admin/local/clear-cache", timeout=60)


def refresh_stats(base):
    """Refresh planner statistics once, before the corpus runs.

    Cold CACHES are the point of this harness; cold STATISTICS are not.  A
    corpus measured against a planner that thinks a 15M-row table holds a few
    hundred rows records latencies for plans no healthy deployment would ever
    choose — this harness once published a 95.3 s baseline for work that takes
    21.6 s.  Loopback-only, so it is skipped (with a note) when the endpoint is
    unreachable, e.g. when driving the compose stack from the host."""
    try:
        post(base, "/admin/local/analyze", timeout=600)
        print("planner statistics refreshed")
    except Exception as e:  # noqa: BLE001 - diagnostic only
        print(f"WARNING: could not refresh planner statistics ({e});"
              " latencies may reflect stale plans")


def run_query(base, q):
    t0 = time.perf_counter()
    body = post(base, "/query?format=markdown&projection=names", q.encode())
    dt = time.perf_counter() - t0
    err = body.startswith("# Error")
    stats = ""
    lines = body.splitlines()
    for i, line in enumerate(lines):
        if line.strip() == "# Stats" and i + 1 < len(lines):
            stats = lines[i + 1].strip()
            break
    if err:  # first line of the fenced error message
        stats = next((l.strip() for l in lines[1:] if l.strip() and l.strip() != "```text"), "error")
    return dt, err, stats


def log_offset(path):
    """Current end-of-file byte offset, or 0 if the log isn't there yet."""
    if not path:
        return 0
    try:
        return Path(path).stat().st_size
    except OSError:
        return 0


def read_log_delta(path, start):
    """Bytes appended to `path` since `start` (best-effort; empty on any error)."""
    if not path:
        return ""
    try:
        with open(path, "r", errors="replace") as f:
            f.seek(start)
            return f.read()
    except OSError:
        return ""


def _num_after(line, key):
    """Integer immediately following `key` in `line`, or None. No regex."""
    i = line.find(key)
    if i < 0:
        return None
    j = i + len(key)
    k = j
    while k < len(line) and line[k].isdigit():
        k += 1
    return int(line[j:k]) if k > j else None


def max_sql_sizes(text):
    """(max result_rows, max result_bytes) across every SQL span in `text`."""
    rows = bytes_ = None
    for line in text.splitlines():
        r = _num_after(line, "result_rows=")
        if r is not None:
            rows = r if rows is None else max(rows, r)
        b = _num_after(line, "result_bytes=")
        if b is not None:
            bytes_ = b if bytes_ is None else max(bytes_, b)
    return rows, bytes_


def human_bytes(n):
    if n is None:
        return "—"
    x = float(n)
    for unit in ("B", "KB", "MB", "GB"):
        if x < 1024 or unit == "GB":
            return f"{x:.0f} {unit}" if unit == "B" else f"{x:.1f} {unit}"
        x /= 1024
    return f"{x:.1f} GB"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repeats", type=int, default=1)
    ap.add_argument("--base", default="http://127.0.0.1:8099")
    ap.add_argument("--logfile", default=None,
                    help="askld log path; enables per-query max result_rows/bytes")
    ap.add_argument("--clear-cmd", default=None,
                    help="shell command to clear caches before each run "
                         "(compose stack: psql DELETE of ephemeral layers)")
    ap.add_argument("--log-settle", type=float, default=0.4,
                    help="seconds to wait after a query before reading the log "
                         "delta (lets `docker logs -f` flush the last lines)")
    args = ap.parse_args()

    queries = [l.strip() for l in (HERE / "queries.txt").read_text().splitlines()
               if l.strip() and not l.startswith("#")]

    # Once, before the corpus: caches must be cold, statistics must not be.
    refresh_stats(args.base)

    rows = []
    for q in queries:
        best, err, stats = float("inf"), False, ""
        max_rows = max_bytes = None
        for _ in range(args.repeats):
            try:
                clear_cache(args.base, args.clear_cmd)
            except Exception as e:
                print(f"  (clear-cache failed: {e})")
            off = log_offset(args.logfile)  # snapshot AFTER clear-cache, before the query
            try:
                dt, err, stats = run_query(args.base, q)
            except Exception as e:
                dt, err, stats = float("inf"), True, f"request failed: {str(e)[:60]}"
            best = min(best, dt)
            if args.logfile:
                time.sleep(args.log_settle)  # let the follower flush this query's lines
            r, b = max_sql_sizes(read_log_delta(args.logfile, off))
            if r is not None:
                max_rows = r if max_rows is None else max(max_rows, r)
            if b is not None:
                max_bytes = b if max_bytes is None else max(max_bytes, b)
        rows.append((best, err, q, stats, max_rows, max_bytes))
        ms = "TIMEOUT" if best == float("inf") else f"{best * 1000:8.0f} ms"
        sz = f"  [{max_rows if max_rows is not None else '—'} rows, {human_bytes(max_bytes)}]" if args.logfile else ""
        print(f"{ms}  {'ERR ' if err else '    '}{q[:52]}{sz}", flush=True)

    rows.sort(key=lambda r: (-1 if r[0] == float("inf") else r[0]), reverse=True)
    out = ["# askl query performance — cold latency (cache-off staging)\n",
           f"Cache-off askld vs the compose DB; **both caches cleared before each run** "
           f"(RAM + eph via `/admin/clear-cache`); {args.repeats} cold run(s)/query, best time. "
           f"`rows`/`bytes` = max any single SQL returned (from `--logfile`).\n",
           "| latency | max SQL rows | max SQL bytes | err | stats | query |",
           "|--:|--:|--:|:--:|---|---|"]
    for best, err, q, stats, mr, mb in rows:
        lat = "**TIMEOUT**" if best == float("inf") else f"{best * 1000:.0f} ms"
        out.append(f"| {lat} | {mr if mr is not None else '—'} | {human_bytes(mb)} "
                   f"| {'⚠️' if err else ''} | {stats} | `{q}` |")
    (HERE / "REPORT.md").write_text("\n".join(out) + "\n")
    print("\nwrote", HERE / "REPORT.md")


if __name__ == "__main__":
    main()
