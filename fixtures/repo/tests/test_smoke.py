import unittest
import tempfile
from pathlib import Path

from src.config_loader import load_config
from src.manifest import parse_manifest
from src.planner import build_release_order
from src.tiny_tasks import clamp, median, slugify


class SmokeTests(unittest.TestCase):
    def test_clamp_inside_range(self):
        self.assertEqual(clamp(4, 1, 9), 4)

    def test_slugify_words(self):
        self.assertEqual(slugify("Carry Agent"), "carry-agent")

    def test_median_odd(self):
        self.assertEqual(median([3, 1, 2]), 2)

    def test_release_plan_orders_dependencies_first(self):
        packages = parse_manifest("app: core\ncore:\ndocs:")
        self.assertEqual(build_release_order(packages), ["core", "docs", "app"])

    def test_config_loader_merges_an_include(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "base.json").write_text(
                '{"server": {"host": "localhost", "port": 80}}'
            )
            (root / "app.json").write_text(
                '{"include": "base.json", "server": {"port": 8080}}'
            )
            self.assertEqual(
                load_config(root / "app.json"),
                {"server": {"host": "localhost", "port": 8080}},
            )


if __name__ == "__main__":
    unittest.main()
