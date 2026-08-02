#!/usr/bin/env python3
"""askl query performance harness — measure cold per-query latency.

Runs each query in `queries.txt` against a **cache-off** staging askld, clearing
BOTH caches before every run (RAM SQL cache + DB ephemeral layers) via the
loopback `POST /admin/clear-cache` endpoint, so each measurement is cold. Writes
a ranked `REPORT.md`.

Prereq — a staging askld with the SQL cache disabled and a generous timeout:
    ASKL_SQL_CACHE_BYTES=0 ASKL_QUERY_TIMEOUT=120 \
      ASKL_DATABASE_URL=postgres://postgres:postgres@<compose-db>:5432/askl \
      ./target/release/askld serve --host 127.0.0.1 --port 8099
(release build — debug inflates the Rust-side latency).

Usage: python3 run.py [--repeats N] [--base URL]
"""
import argparse
import time
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent


def post(base, path, body=b"", timeout=200):
    req = urllib.request.Request(base + path, data=body, method="POST",
                                 headers={"Content-Type": "text/plain"})
    return urllib.request.urlopen(req, timeout=timeout).read().decode()


def clear_cache(base):
    post(base, "/admin/local/clear-cache", timeout=60)


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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repeats", type=int, default=1)
    ap.add_argument("--base", default="http://127.0.0.1:8099")
    args = ap.parse_args()

    queries = [l.strip() for l in (HERE / "queries.txt").read_text().splitlines()
               if l.strip() and not l.startswith("#")]
    rows = []
    for q in queries:
        best, err, stats = float("inf"), False, ""
        for _ in range(args.repeats):
            try:
                clear_cache(args.base)
            except Exception as e:
                print(f"  (clear-cache failed: {e})")
            try:
                dt, err, stats = run_query(args.base, q)
            except Exception as e:
                dt, err, stats = float("inf"), True, f"request failed: {str(e)[:60]}"
            best = min(best, dt)
        rows.append((best, err, q, stats))
        ms = "TIMEOUT" if best == float("inf") else f"{best * 1000:8.0f} ms"
        print(f"{ms}  {'ERR ' if err else '    '}{q[:58]}", flush=True)

    rows.sort(key=lambda r: (-1 if r[0] == float("inf") else r[0]), reverse=True)
    out = ["# askl query performance — cold latency (cache-off staging)\n",
           f"Cache-off askld vs the compose DB; **both caches cleared before each run** "
           f"(RAM + eph via `/admin/clear-cache`); {args.repeats} cold run(s)/query, best time.\n",
           "| latency | err | stats | query |", "|--:|:--:|---|---|"]
    for best, err, q, stats in rows:
        lat = "**TIMEOUT**" if best == float("inf") else f"{best * 1000:.0f} ms"
        out.append(f"| {lat} | {'⚠️' if err else ''} | {stats} | `{q}` |")
    (HERE / "REPORT.md").write_text("\n".join(out) + "\n")
    print("\nwrote", HERE / "REPORT.md")


if __name__ == "__main__":
    main()
