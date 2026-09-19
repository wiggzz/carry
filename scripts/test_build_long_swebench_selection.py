#!/usr/bin/env python3
import importlib.util
import json
import pathlib
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).with_name("build_long_swebench_selection.py")


class LongTrajectorySelectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("build_long_swebench_selection", SCRIPT)
        cls.builder = importlib.util.module_from_spec(spec)
        assert spec.loader
        spec.loader.exec_module(cls.builder)

    def test_build_manifest_ranks_api_calls_descending_then_instance_id(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for instance_id, calls, cost in (
                ("repo__b-2", 10, 0.2), ("repo__a-1", 10, 0.1), ("repo__c-3", 9, 0.3),
            ):
                path = root / instance_id / f"{instance_id}.traj.json"
                path.parent.mkdir()
                path.write_text(json.dumps({
                    "instance_id": instance_id,
                    "trajectory_format": "mini-swe-agent-1.1",
                    "info": {"model_stats": {"api_calls": calls, "instance_cost": cost}},
                }))
            manifest, smoke = self.builder.build_manifests(
                root, source_dataset="princeton-nlp/SWE-bench_Verified",
                source_manifest_sha256="a" * 64, selection_size=2, smoke_size=1,
            )
            self.assertEqual(manifest["instance_ids"], ["repo__a-1", "repo__b-2"])
            self.assertEqual(smoke["instance_ids"], ["repo__a-1"])
            self.assertEqual([row["api_calls"] for row in manifest["ranking"]], [10, 10])
            self.assertEqual(manifest["trajectory_source"]["measurement_count"], 3)
            self.assertEqual(manifest["trajectory_source"]["rank_order"], "api_calls desc, instance_id asc")

    def test_build_manifest_rejects_missing_or_duplicate_instance_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "one.traj.json").write_text(json.dumps({"instance_id": "x", "info": {"model_stats": {"api_calls": 1, "instance_cost": 0.1}}}))
            (root / "two.traj.json").write_text(json.dumps({"instance_id": "x", "info": {"model_stats": {"api_calls": 2, "instance_cost": 0.2}}}))
            with self.assertRaisesRegex(ValueError, "duplicate"):
                self.builder.build_manifests(root, source_dataset="x", source_manifest_sha256="a" * 64, selection_size=1, smoke_size=1)


if __name__ == "__main__":
    unittest.main()
