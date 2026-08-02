#!/usr/bin/env python3
"""Regression guard: a timed-out search() must NOT poison the connection pool.

Background: a `search()` on a common term outruns the query timeout. The tokio
timeout cancels the execute future and `EphTransaction::Drop` cannot async-ROLLBACK,
so a pooled connection is briefly left `idle in transaction (aborted)`. The bb8
pool's `RecyclingMethod::CustomQuery("ROLLBACK")` (index_impl.rs) rolls it back
before the next request runs a query on it, so the abort never reaches a client.
The eval once saw this cascade ("current transaction is aborted") 24x on an older
build; this script locks in that it no longer does.

It does NOT reproduce a bug on a healthy build — a passing run is the expected
result. It exists so a future change that breaks the recycling (e.g. flipping the
`RecyclingMethod`, or returning a connection without rollback) is caught.

Prereq — a **short**-timeout deploy so search() actually times out (the opposite
of run.py's long-timeout latency staging). The compose server redeployed with
`ASKL_QUERY_TIMEOUT=5` is exactly this:
    ASKL_QUERY_TIMEOUT=5 ./target/release/askld serve ...   # or the :3002 deploy

Usage: python3 poison_check.py [--base URL]   (default http://localhost:3002)
Exit 0 = no cascade (pass); exit 1 = a cheap query hit an aborted/failed
connection (regression), or the deploy timeout is too high to exercise it.
"""
import argparse
import sys
import threading
import time
import urllib.request

# Markers that mean a connection was poisoned / an operation failed mid-request.
CASCADE_MARKERS = (
    "current transaction is aborted",
    "Failed to load",
    "Failed to query",
    "Failed to resolve",
)
# Cheap structured queries — fast, deterministic, hit the linux index. None of
# these should ever error on a healthy server.
CHEAP_QUERIES = [
    'g"*drm_mm_init*"',
    'g"*drm_mm_takedown*"',
    'g"*drm_mm_init*" { }',  # containment: exercises symbols + parents + children
]
SLOW_QUERY = 'search("color")'  # common term → outruns a short timeout


def post(base, q, timeout=60):
    req = urllib.request.Request(
        base + "/query?format=markdown&projection=names",
        data=q.encode(),
        method="POST",
        headers={"Content-Type": "text/plain"},
    )
    try:
        return urllib.request.urlopen(req, timeout=timeout).read().decode()
    except urllib.error.HTTPError as e:
        # askld serves a query timeout as HTTP 504 (and errors as 4xx/5xx) but
        # still puts the markdown `# Error ...` body in the response — keep it.
        return e.read().decode()
    except Exception as e:  # connection refused / DNS / socket timeout = real failure
        return f"# Error\nrequest failed: {str(e)[:80]}"


def is_cascade(body):
    return any(m in body for m in CASCADE_MARKERS)


def cheap_burst(base, n):
    """Fire n cheap queries; return the list of (query, body) that cascaded."""
    bad = []
    for i in range(n):
        q = CHEAP_QUERIES[i % len(CHEAP_QUERIES)]
        body = post(base, q)
        if is_cascade(body) or body.startswith("# Error"):
            bad.append((q, body.strip().replace("\n", " ")[:120]))
    return bad


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://localhost:3002")
    ap.add_argument("--saturate", type=int, default=12,
                    help="concurrent timing-out searches to saturate the pool")
    ap.add_argument("--burst", type=int, default=30,
                    help="cheap queries fired immediately after saturation")
    args = ap.parse_args()
    failures = []

    # 1. Confirm search() actually times out cleanly (not an abort, not a success).
    print(f"[1] {SLOW_QUERY} — expect a clean timeout error ...", flush=True)
    body = post(args.base, SLOW_QUERY)
    if "current transaction is aborted" in body:
        failures.append("search() itself returned 'transaction is aborted' (already poisoned)")
        print("    FAIL: returned 'transaction is aborted'")
    elif "time limit" in body and body.startswith("# Error"):
        print("    ok: clean timeout error")
    else:
        # No timeout → the deploy's ASKL_QUERY_TIMEOUT is too high to exercise
        # this guard. Treat as an error so it isn't a false green.
        head = body.strip().replace("\n", " ")[:100]
        failures.append(f"search() did not time out (need a short-timeout deploy). Got: {head}")
        print(f"    FAIL: did not time out — got: {head}")

    # 2. Sequential cheap queries right after the timeout.
    print("[2] cheap queries after a single timeout ...", flush=True)
    bad = cheap_burst(args.base, len(CHEAP_QUERIES) * 3)
    if bad:
        failures.append(f"{len(bad)} cheap queries cascaded after single timeout")
        for q, b in bad[:3]:
            print(f"    FAIL: {q} -> {b}")
    else:
        print("    ok: all clean")

    # 3. Worst case: saturate the whole pool with concurrent timeouts, then fire
    #    a burst the instant the transactions abort.
    print(f"[3] saturate pool ({args.saturate} concurrent timeouts) then burst {args.burst} ...", flush=True)
    threads = [threading.Thread(target=post, args=(args.base, SLOW_QUERY))
               for _ in range(args.saturate)]
    for t in threads:
        t.start()
    time.sleep(5.2)  # just past a 5s statement_timeout — transactions now aborted
    bad = cheap_burst(args.base, args.burst)
    for t in threads:
        t.join()
    if bad:
        failures.append(f"{len(bad)}/{args.burst} cheap queries cascaded during pool saturation")
        for q, b in bad[:3]:
            print(f"    FAIL: {q} -> {b}")
    else:
        print(f"    ok: {args.burst}/{args.burst} clean")

    print()
    if failures:
        print("POISON REGRESSION DETECTED:")
        for f in failures:
            print(f"  - {f}")
        sys.exit(1)
    print("PASS: no cascade — timed-out search() does not poison the pool.")


if __name__ == "__main__":
    main()
