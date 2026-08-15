#!/usr/bin/env python3
"""Tests for the portable GitHub Actions SWE-bench runner."""
import importlib.util
import pathlib
import unittest

SCRIPT = pathlib.Path(__file__).with_name("swebench_benchmark.py")


class BenchmarkConfigurationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("swebench_benchmark", SCRIPT)
        cls.runner = importlib.util.module_from_spec(spec)
        assert spec.loader
        spec.loader.exec_module(cls.runner)

    def test_shards_partition_each_selected_task_once_in_frozen_order(self):
        tasks = [{"instance_id": f"task-{i}"} for i in range(11)]
        shards = [self.runner.select_shard(tasks, index, 5) for index in range(5)]
        self.assertEqual(
            [task["instance_id"] for shard in shards for task in shard],
            [task["instance_id"] for task in tasks],
        )

    def test_merged_records_require_exact_task_method_denominator(self):
        tasks = [{"instance_id": "one"}, {"instance_id": "two"}]
        records = [
            {"instance_id": "one", "method": "carry"},
            {"instance_id": "one", "method": "codex"},
            {"instance_id": "two", "method": "carry"},
            {"instance_id": "two", "method": "codex"},
        ]
        self.runner.validate_merged_records(tasks, records, ("carry", "codex"))

    def test_verified_config_pins_the_canonical_dataset_revision(self):
        config = self.runner.load_verified_config()
        self.assertEqual(config["dataset"]["name"], "princeton-nlp/SWE-bench_Verified")
        self.assertEqual(config["dataset"]["revision"], "c104f840cc67f8b6eec6f759ebc8b2693d585d4a")
        self.assertEqual(config["dataset"]["task_count"], 500)
        self.assertEqual(set(config["presets"]), {"smoke-5", "selected-50", "verified-full"})

    def test_seeded_subset_is_deterministic_and_independent_of_input_order(self):
        ids = ["zeta", "alpha", "gamma", "beta"]
        expected = self.runner.select_seeded_subset(ids, task_count=2, seed="carry-v1")
        self.assertEqual(expected, self.runner.select_seeded_subset(list(reversed(ids)), task_count=2, seed="carry-v1"))
        self.assertEqual(len(expected), 2)
        self.assertTrue(set(expected).issubset(ids))

    def test_seeded_subset_rejects_out_of_range_task_count(self):
        with self.assertRaisesRegex(ValueError, "task count"):
            self.runner.select_seeded_subset(["one"], task_count=2, seed="carry-v1")

    def test_merged_records_reject_duplicate_task_input(self):
        tasks = [{"instance_id": "same"}, {"instance_id": "same"}]
        records = [
            {"instance_id": "same", "method": "carry"},
            {"instance_id": "same", "method": "codex"},
        ]
        with self.assertRaisesRegex(ValueError, "duplicate task"):
            self.runner.validate_merged_records(tasks, records, ("carry", "codex"))

    def test_merged_records_reject_duplicate_method_input(self):
        tasks = [{"instance_id": "one"}]
        records = [{"instance_id": "one", "method": "carry"}]
        with self.assertRaisesRegex(ValueError, "duplicate method"):
            self.runner.validate_merged_records(tasks, records, ("carry", "carry"))

    def test_merged_records_reject_malformed_record(self):
        tasks = [{"instance_id": "one"}]
        with self.assertRaisesRegex(ValueError, "invalid instance_id"):
            self.runner.validate_merged_records(tasks, [{"instance_id": [], "method": "carry"}], ("carry",))

    def test_committed_selection_has_five_ordered_ten_task_shards(self):
        selection = self.runner.load_selection()
        shards = [self.runner.select_shard([{"instance_id": task} for task in selection], index, 5) for index in range(5)]
        self.assertEqual([len(shard) for shard in shards], [10, 10, 10, 10, 10])
        self.assertEqual([item["instance_id"] for shard in shards for item in shard], selection)
        self.assertEqual(self.runner.sha256_file(self.runner.DEFAULT_SELECTION), "d26efa7d55df331566a69aa15c4cbc78c044100f6c6c73610f0d7a0b19bb3877")

    def test_merged_records_reject_duplicate_or_missing_pairs(self):
        tasks = [{"instance_id": "one"}, {"instance_id": "two"}]
        records = [
            {"instance_id": "one", "method": "carry"},
            {"instance_id": "one", "method": "carry"},
            {"instance_id": "one", "method": "codex"},
        ]
        with self.assertRaisesRegex(ValueError, "expected exactly"):
            self.runner.validate_merged_records(tasks, records, ("carry", "codex"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
