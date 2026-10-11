#!/usr/bin/env python3
"""Native exact label permutation inference; no Core dependency or API."""

import argparse
import csv
from collections import Counter
from fractions import Fraction
import hashlib
import io
import itertools
import json
import math
import os
from pathlib import Path
import sys
import time


def publish(path, value):
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("w", encoding="utf-8") as stream:
        json.dump(value, stream, sort_keys=True)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(temporary, path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--max-permutations", type=int, default=200_000_000,
                        help="CPU-work limit; reject before enumeration if exceeded")
    parser.add_argument("--progress-every", type=int, default=100_000)
    args = parser.parse_args()
    if args.max_permutations < 1 or args.progress_every < 1:
        parser.error("limits must be positive")
    # Observe native entry before any existing-result guard can reject a replay.
    # This file is evidence from the worker, not an external exactly-once lock.
    launch = {"pid": os.getpid(), "started_at_ns": time.time_ns()}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    attempts = args.output.with_name(args.output.name + ".launch-attempts.jsonl")
    with attempts.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(launch, sort_keys=True) + "\n")
        stream.flush()
        os.fsync(stream.fileno())
    # A fresh directory prevents accidentally overwriting a prior result.
    args.output.mkdir(parents=True, exist_ok=False)
    started = time.monotonic()
    publish(args.output / "launch.json", launch)
    with args.input.open("rb") as stream:
        raw = stream.read(128 * 1024 + 1)
    if len(raw) > 128 * 1024:
        raise ValueError("input exceeds 128 KiB; protects parser memory")
    rows = list(csv.DictReader(io.StringIO(raw.decode("utf-8"))))
    if not rows or len(rows) > 32:
        raise ValueError("expected 1 to 32 observations")
    if any(set(row) != {"row", "species", "sepal_length_tenths_cm"} for row in rows):
        raise ValueError("unexpected CSV schema")
    if len({row["row"] for row in rows}) != len(rows):
        raise ValueError("duplicate observation identity")
    left = [int(row["sepal_length_tenths_cm"]) for row in rows if row["species"] == "versicolor"]
    right = [int(row["sepal_length_tenths_cm"]) for row in rows if row["species"] == "virginica"]
    if len(left) != len(right) or not left or len(left) + len(right) != len(rows):
        raise ValueError("expected equal nonempty versicolor and virginica groups")
    if any(abs(value) > 10_000 for value in left + right):
        raise ValueError("measurement magnitude exceeds 10000 tenths cm")
    values = left + right
    n = len(left)
    total = math.comb(len(values), n)
    if total > args.max_permutations:
        raise ValueError(f"{total} permutations exceed --max-permutations={args.max_permutations}")
    observed = abs(sum(right) - sum(left))
    pooled_sum = sum(values)
    distribution = Counter()
    extreme = 0
    completed = 0
    progress = {"complete": False, "input_sha256": hashlib.sha256(raw).hexdigest(),
                "total_assignments": total, "completed_assignments": 0}
    publish(args.output / "progress.json", progress)
    for combination in itertools.combinations(values, n):
        selected_sum = sum(combination)
        difference = abs(2 * selected_sum - pooled_sum)
        distribution[difference] += 1
        extreme += difference >= observed
        completed += 1
        if completed % args.progress_every == 0:
            progress["completed_assignments"] = completed
            publish(args.output / "progress.json", progress)
            print(json.dumps({"progress": completed, "total": total}), flush=True)
    effect = Fraction(sum(right) - sum(left), 10 * n)
    p_value = Fraction(extreme, total)
    result = {
        "format": "iris-exact-permutation-v1", "complete": True,
        "input_sha256": hashlib.sha256(raw).hexdigest(),
        "groups": {"versicolor": {"n": n, "sum_tenths_cm": sum(left)},
                   "virginica": {"n": n, "sum_tenths_cm": sum(right)}},
        "statistic": "absolute difference of group sums in tenths cm",
        "observed_statistic": observed,
        "effect_virginica_minus_versicolor_cm": {"numerator": effect.numerator, "denominator": effect.denominator},
        "total_assignments": total, "completed_assignments": completed,
        "extreme_assignments": extreme,
        "p_value": {"numerator": p_value.numerator, "denominator": p_value.denominator},
        "distribution": {str(key): distribution[key] for key in sorted(distribution)},
        "elapsed_seconds": time.monotonic() - started,
    }
    publish(args.output / "result.json", result)
    progress.update(complete=True, completed_assignments=completed)
    publish(args.output / "progress.json", progress)
    print(json.dumps({"result": "result.json", "total": total,
                      "extreme": extreme, "p_value": str(p_value), "effect_cm": str(effect)}), flush=True)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
