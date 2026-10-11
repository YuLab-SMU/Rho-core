#!/usr/bin/env python3
"""Small analytic and negative controls for the independent scientific oracle."""

import copy
import csv
from fractions import Fraction
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

from verify import oracle, verify

HERE = Path(__file__).resolve().parent


def write_input(path, left, right):
    with path.open("w", newline="") as stream:
        writer = csv.writer(stream)
        writer.writerow(["row", "species", "sepal_length_tenths_cm"])
        for index, value in enumerate(left + right):
            writer.writerow([index + 1, "versicolor" if index < len(left) else "virginica", value])


class ScienceControls(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="iris-science-")
        self.root = Path(self.temporary.name)
        self.input = self.root / "input.csv"
        write_input(self.input, [1, 2, 2], [3, 4, 5])
        run = subprocess.run([sys.executable, str(HERE / "analyze.py"), "--input", str(self.input),
                              "--output", str(self.root / "native")], capture_output=True, text=True, timeout=10)
        self.assertEqual(run.returncode, 0, run.stderr)
        self.result_path = self.root / "native/result.json"
        self.result = json.loads(self.result_path.read_text())

    def tearDown(self):
        self.temporary.cleanup()

    def mutated(self, result):
        path = self.root / "mutated.json"
        path.write_text(json.dumps(result))
        return path

    def test_native_enumeration_matches_independent_oracle_with_ties(self):
        self.assertTrue(verify(self.input, self.result_path)["verified"])

    def test_closed_form_small_case(self):
        # Six assignments of [1,2,3,4]: absolute sum differences 4,2,0,0,2,4.
        write_input(self.input, [1, 2], [3, 4])
        expected = oracle(self.input)
        self.assertEqual(expected["distribution"], {"0": 2, "2": 2, "4": 2})
        self.assertEqual(expected["p_value"], {"numerator": 1, "denominator": 3})
        self.assertEqual(expected["effect_virginica_minus_versicolor_cm"], {"numerator": 1, "denominator": 5})

    def test_p_value_count_distribution_and_unfinished_mutations_rejected(self):
        mutations = []
        for key in ["extreme_assignments", "completed_assignments", "observed_statistic"]:
            result = copy.deepcopy(self.result)
            result[key] += 1
            mutations.append((key, result))
        result = copy.deepcopy(self.result)
        result["p_value"]["numerator"] += 1
        mutations.append(("p_value", result))
        result = copy.deepcopy(self.result)
        first = next(iter(result["distribution"]))
        result["distribution"][first] += 1
        mutations.append(("distribution", result))
        result = copy.deepcopy(self.result)
        result["complete"] = False
        mutations.append(("complete", result))
        for key, result in mutations:
            with self.subTest(key=key), self.assertRaises(AssertionError):
                verify(self.input, self.mutated(result))

    def test_raw_input_tamper_rejected_even_when_measurements_unchanged(self):
        # An extra blank line preserves all numerical input and reported metadata.
        self.input.write_bytes(self.input.read_bytes() + b"\n")
        with self.assertRaisesRegex(AssertionError, "input_sha256"):
            verify(self.input, self.result_path)

    def test_transformation_invariants(self):
        baseline = oracle(self.input)
        for translation, scale in [(17, 1), (-5, 3)]:
            write_input(self.input, [scale * v + translation for v in [1, 2, 2]],
                        [scale * v + translation for v in [3, 4, 5]])
            transformed = oracle(self.input)
            self.assertEqual(transformed["p_value"], baseline["p_value"])
            self.assertEqual(transformed["distribution"],
                             {str(int(key) * scale): value for key, value in baseline["distribution"].items()})
            effect = transformed["effect_virginica_minus_versicolor_cm"]
            original_effect = baseline["effect_virginica_minus_versicolor_cm"]
            self.assertEqual(Fraction(effect["numerator"], effect["denominator"]),
                             scale * Fraction(original_effect["numerator"], original_effect["denominator"]))
        write_input(self.input, [3, 4, 5], [1, 2, 2])
        swapped = oracle(self.input)
        self.assertEqual(swapped["p_value"], baseline["p_value"])
        self.assertEqual(swapped["distribution"], baseline["distribution"])
        self.assertEqual(swapped["effect_virginica_minus_versicolor_cm"]["numerator"],
                         -baseline["effect_virginica_minus_versicolor_cm"]["numerator"])

    def test_compute_limit_rejects_without_a_final_result(self):
        output = self.root / "limited"
        result = subprocess.run([sys.executable, str(HERE / "analyze.py"), "--input", str(self.input),
                                 "--output", str(output), "--max-permutations", "1"],
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 1)
        self.assertIn("exceed --max-permutations=1", result.stderr)
        self.assertFalse((output / "result.json").exists())

    def test_launch_attempt_is_observed_before_existing_output_rejection(self):
        output = self.root / "native"
        attempts = self.root / "native.launch-attempts.jsonl"
        first = attempts.read_text().splitlines()
        self.assertEqual(len(first), 1)
        repeat = subprocess.run([sys.executable, str(HERE / "analyze.py"), "--input", str(self.input),
                                 "--output", str(output)], capture_output=True, text=True, timeout=10)
        self.assertEqual(repeat.returncode, 1)
        observed = attempts.read_text().splitlines()
        self.assertEqual(len(observed), 2)
        self.assertEqual(observed[0], first[0])
        self.assertNotEqual(json.loads(observed[0])["pid"], json.loads(observed[1])["pid"])

    def test_oversized_input_rejected_by_both_native_and_oracle(self):
        self.input.write_bytes(b"x" * (128 * 1024 + 1))
        with self.assertRaisesRegex(AssertionError, "input size"):
            oracle(self.input)
        output = self.root / "oversized"
        run = subprocess.run([sys.executable, str(HERE / "analyze.py"), "--input", str(self.input),
                              "--output", str(output)], capture_output=True, text=True, timeout=10)
        self.assertEqual(run.returncode, 1)
        self.assertIn("input exceeds 128 KiB", run.stderr)
        self.assertFalse((output / "result.json").exists())

    def test_pinned_iris_rows_match_installed_r(self):
        r = shutil.which("R")
        if not r:
            self.skipTest("R not installed; scientific runtime itself only needs Python")
        expression = r'''d <- read.csv(commandArgs(TRUE)[1]); stopifnot(identical(d$row, c(51:65, 101:115))); stopifnot(identical(as.character(d$species), as.character(iris$Species[d$row]))); stopifnot(all(d$sepal_length_tenths_cm == round(10*iris$Sepal.Length[d$row]))); cat(as.character(packageVersion("datasets")), "\n")'''
        result = subprocess.run([r, "--vanilla", "--slave", "-e", expression, "--args", str(HERE / "iris.csv")],
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
