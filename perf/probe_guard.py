#!/usr/bin/env python3
"""Regression guard: no cardinality probe may be slow, least of all an EMPTY one.

Background. `mod("amdgpu") { macro { func } }` on the kernel took 11.4 s, and
all of it was ONE probe that returned ZERO rows in 11,153 ms while the other
three roles in the same concurrent wave took 21, 87 and 146 ms. Two causes,
harmless alone and lethal together:

  * a REFS role bound as `si.id = ANY($ids)` -- postgres estimates a
    parameterised array at a fixed 10 rows (measured), so a cap-sized role
    (~1000 ids) looked 100x smaller than it was and the planner drove the
    semi-join from the OUTER side: every symbol in the project, 9M rows;
  * `LIMIT cap + 1` only rescues that plan when rows EXIST. A probe that finds
    1001 rows stops at once -- which is why the capped roles were the fast ones.
    A probe that finds NOTHING cannot stop early, so proving emptiness exhausted
    all 9M rows.

Fixed in askl `0d2b713` by emitting REFS roles as MATERIALIZED CTEs, as the HAS
roles already were (perf/PROBE_BASELINE.md S7 -> S7c records the same 10.5 s ->
0.5 ms collapse). This script exists because `perf/probe_baseline.sql` had never
tested a REFS role with a cap-sized array, nor an EMPTY answer for any role --
the two gaps that let the bug ship -- and because a timing threshold on the
whole query would have passed it for months on ucx, where roles are small.

What it asserts, from askld's OWN log lines
(`probe completed elapsed_ms=N rows=R capped=C` -- the signal that finally found
the bug after two wrong diagnoses from derived numbers):

  1. no single probe exceeds --max-probe-ms (default 2000);
  2. the empty-answer pathology specifically: the slowest `rows=0` probe is not
     more than --empty-ratio times the slowest non-empty one (default 5x).

Prereq -- a COLD askld. There is no cache-clear endpoint on the compose deploy,
and a warm SQL cache serves every probe in ~0 ms, which would pass trivially. So
the guard refuses a false green: if every probe it saw took < --warm-ms (default
5) it exits 2 ("indeterminate -- cache warm, restart askld") rather than 0.
Restart with:  ( cd compose && docker compose restart askld )

Usage: python3 probe_guard.py [--base URL] [--compose-dir DIR] [--max-probe-ms N]
Exit 0 = pass; 1 = a probe regressed; 2 = indeterminate (warm cache / no probes).
"""
import argparse
import subprocess
import sys
import urllib.request
from pathlib import Path

# Each query exercises a probe shape the baseline never covered. Comments say
# which role and why it is there; keep the forced-empty one -- it is the bug.
QUERIES = [
    # cap-sized REFS role (mod("amdgpu") resolves to 931 instances) AND the
    # empty-answer probe: this is the exact query that took 11.4 s.
    ('project("linux") { mod("amdgpu") { macro { func } } }', "cap-sized REFS role, contains an empty probe"),
    # CONTROL, not the pathology: "zzz" is a selective indexed name, so its probe
    # short-circuits before the role matters. The pathological empty -- an
    # UNSELECTIVE outer side disjoint from the role -- is inside query #1.
    ('project("linux") { mod("amdgpu") { func { "zzz_no_such_symbol_zzz" } } }', "control: selective empty"),
    # cap-sized REFS role, small non-empty answer (40 symbols)
    ('project("linux") { mod("amdgpu") { func { "drm_dev_enter" } } }', "cap-sized REFS role, small answer"),
    # second module, so the guard is not fitted to amdgpu alone
    ('project("linux") { mod("nouveau") { macro { func } } }', "cap-sized REFS role, other module"),
    # a HAS (containment) role: a file is a container
    ('project("linux") { file("/linux/fs/read_write.c") { func } }', "HAS role, container"),
]


def post(base, q, timeout=120):
    req = urllib.request.Request(base + "/query", data=q.encode(), method="POST",
                                 headers={"Content-Type": "text/plain"})
    try:
        return urllib.request.urlopen(req, timeout=timeout).read().decode()
    except urllib.error.HTTPError as e:
        return e.read().decode()
    except Exception as e:
        return '{"message":"request failed: %s"}' % str(e)[:80]


def compose_logs(compose_dir):
    """askld's log, newest last. The deploy writes no file; this is the source."""
    out = subprocess.run(
        ["docker", "compose", "-f", "compose.yaml", "-f", "compose.dev.yaml",
         "logs", "askld", "--no-log-prefix"],
        cwd=compose_dir, capture_output=True, text=True, errors="replace")
    return out.stdout.splitlines()


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


def probes_in(lines):
    """[(elapsed_ms, rows, capped)] for every `probe completed` line."""
    out = []
    for line in lines:
        if "probe completed" not in line:
            continue
        ms = _num_after(line, "elapsed_ms=")
        rows = _num_after(line, "rows=")
        if ms is None or rows is None:
            continue
        out.append((ms, rows, "capped=true" in line))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://localhost:3002")
    ap.add_argument("--compose-dir",
                    default=str(Path(__file__).resolve().parents[2] / "compose"))
    ap.add_argument("--max-probe-ms", type=int, default=2000)
    ap.add_argument("--empty-ratio", type=float, default=5.0,
                    help="slowest rows=0 probe may be at most this x the slowest non-empty one")
    ap.add_argument("--warm-ms", type=int, default=5,
                    help="if EVERY probe is under this, the cache is warm: exit 2, not 0")
    args = ap.parse_args()

    failures = []
    all_probes = []
    print(f"{'ms':>7} {'rows':>5} {'capped':>6}  query")
    for q, why in QUERIES:
        before = len(compose_logs(args.compose_dir))
        body = post(args.base, q)
        if '"message"' in body[:40]:
            failures.append(f"{q}: {body[:100]}")
            print(f"  ERROR  {q} -> {body[:80]}")
            continue
        new = compose_logs(args.compose_dir)[before:]
        probes = probes_in(new)
        if not probes:
            print(f"      -     -      -  {q}   (no probe lines seen)")
            continue
        all_probes.extend(probes)
        worst = max(probes)
        print(f"{worst[0]:>7} {worst[1]:>5} {str(worst[2]).lower():>6}  {q}   [{why}; {len(probes)} probes]")
        for ms, rows, capped in probes:
            if ms > args.max_probe_ms:
                failures.append(f"{q}: probe took {ms} ms (rows={rows}, capped={capped}) > {args.max_probe_ms}")

    print()
    if not all_probes:
        print("INDETERMINATE: saw no `probe completed` lines -- is this the compose askld, with RUST_LOG=info?")
        sys.exit(2)
    if max(ms for ms, _, _ in all_probes) < args.warm_ms:
        print(f"INDETERMINATE: every probe < {args.warm_ms} ms -- the SQL cache is warm and this "
              f"measured nothing. Restart askld (`docker compose restart askld`) and rerun.")
        sys.exit(2)

    empty = [ms for ms, rows, _ in all_probes if rows == 0]
    full = [ms for ms, rows, _ in all_probes if rows > 0]
    if empty and full:
        worst_empty, worst_full = max(empty), max(full)
        print(f"slowest empty probe {worst_empty} ms vs slowest non-empty {worst_full} ms")
        if worst_empty > args.empty_ratio * max(worst_full, 1):
            failures.append(f"empty-answer pathology: slowest rows=0 probe {worst_empty} ms is "
                            f">{args.empty_ratio}x the slowest non-empty probe {worst_full} ms")

    if failures:
        print("PROBE REGRESSION DETECTED:")
        for f in failures:
            print(f"  - {f}")
        sys.exit(1)
    print(f"PASS: {len(all_probes)} probes, none over {args.max_probe_ms} ms, empty answers not slower than full ones.")


if __name__ == "__main__":
    main()
