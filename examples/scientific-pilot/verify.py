#!/usr/bin/env python3
"""Independent subset-sum oracle. Does not import the enumeration worker."""

import argparse
from collections import Counter
import csv
from fractions import Fraction
import hashlib
import io
import json
import math
from pathlib import Path


def require(condition, description):
    if not condition:
        raise AssertionError(description)


def oracle(input_path):
    with Path(input_path).open("rb") as stream:
        raw = stream.read(128 * 1024 + 1)
    require(len(raw) <= 128 * 1024, "input size")
    rows = list(csv.DictReader(io.StringIO(raw.decode("utf-8"))))
    require(0 < len(rows) <= 32, "observation count")
    require(all(set(row) == {"row", "species", "sepal_length_tenths_cm"} for row in rows), "input schema")
    require(len({row["row"] for row in rows}) == len(rows), "observation identities")
    require(all(row["species"] in ("versicolor", "virginica") for row in rows), "species")
    left = [int(row["sepal_length_tenths_cm"]) for row in rows if row["species"] == "versicolor"]
    right = [int(row["sepal_length_tenths_cm"]) for row in rows if row["species"] == "virginica"]
    require(0 < len(left) == len(right), "balanced groups")
    require(all(abs(value) <= 10_000 for value in left + right), "measurement magnitude")
    n = len(left)
    values = left + right
    # Coefficients of product(1 + z*x**value), retaining z powers through n.
    # Each observation is counted independently even when values are tied.
    sums = [Counter() for _ in range(n + 1)]
    sums[0][0] = 1
    for value in values:
        for count in range(n, 0, -1):
            for subtotal, multiplicity in list(sums[count - 1].items()):
                sums[count][subtotal + value] += multiplicity
    distribution = Counter()
    for subtotal, multiplicity in sums[n].items():
        distribution[abs(2 * subtotal - sum(values))] += multiplicity
    total = math.comb(2 * n, n)
    require(sum(distribution.values()) == total, "binomial coefficient mass")
    # Every chosen group has a unique complementary group with equal mass.
    require(all(sums[n][sum(values) - subtotal] == multiplicity
                for subtotal, multiplicity in sums[n].items()), "complement symmetry")
    observed = abs(sum(right) - sum(left))
    extreme = sum(multiplicity for statistic, multiplicity in distribution.items() if statistic >= observed)
    effect = Fraction(sum(right) - sum(left), 10 * n)
    p_value = Fraction(extreme, total)
    return {"input_sha256": hashlib.sha256(raw).hexdigest(),
            "groups": {"versicolor": {"n": n, "sum_tenths_cm": sum(left)},
                       "virginica": {"n": n, "sum_tenths_cm": sum(right)}},
            "observed_statistic": observed,
            "effect_virginica_minus_versicolor_cm": {"numerator": effect.numerator, "denominator": effect.denominator},
            "total_assignments": total, "completed_assignments": total,
            "extreme_assignments": extreme,
            "p_value": {"numerator": p_value.numerator, "denominator": p_value.denominator},
            "distribution": {str(key): distribution[key] for key in sorted(distribution)}}


def verify(input_path, result_path):
    result = json.loads(Path(result_path).read_text())
    require(result.get("format") == "iris-exact-permutation-v1", "result format")
    require(result.get("complete") is True, "unfinished result")
    require(result.get("statistic") == "absolute difference of group sums in tenths cm", "statistic definition")
    expected = oracle(input_path)
    for key, value in expected.items():
        require(result.get(key) == value, f"oracle mismatch: {key}")
    return {"verified": True, "algorithm": "independent integer subset-sum dynamic programming",
            "input_sha256": expected["input_sha256"], "total_assignments": expected["total_assignments"],
            "extreme_assignments": expected["extreme_assignments"], "p_value": expected["p_value"],
            "effect_virginica_minus_versicolor_cm": expected["effect_virginica_minus_versicolor_cm"]}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--result", required=True, type=Path)
    args = parser.parse_args()
    print(json.dumps(verify(args.input, args.result), sort_keys=True))
