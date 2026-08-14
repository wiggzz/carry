import json
import pathlib
import sys
import tempfile
import unittest

repo = pathlib.Path(sys.argv[1])
sys.path.insert(0, str(repo))
from src.config_loader import load_config
from src.config_merge import deep_merge


class ConfigLoaderGrader(unittest.TestCase):
    def test_deep_merge_does_not_alias_or_mutate(self):
        base = {"db": {"host": "db", "ports": [1, 2]}, "keep": True}
        override = {"db": {"ports": [3], "user": "me"}}
        merged = deep_merge(base, override)
        self.assertEqual(
            merged,
            {"db": {"host": "db", "ports": [3], "user": "me"}, "keep": True},
        )
        merged["db"]["ports"].append(4)
        self.assertEqual(base["db"]["ports"], [1, 2])
        self.assertEqual(override["db"]["ports"], [3])

    def test_nested_includes_order_and_expansion(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            (root / "foundation.json").write_text(
                json.dumps(
                    {"db": {"host": "old", "port": 5432}, "tags": ["base"]}
                )
            )
            (root / "region.json").write_text(
                json.dumps(
                    {"include": "foundation.json", "db": {"host": "regional"}}
                )
            )
            (root / "secrets.json").write_text(
                json.dumps({"db": {"password": "prefix-${TOKEN}"}})
            )
            (root / "app.json").write_text(
                json.dumps(
                    {
                        "include": ["region.json", "secrets.json"],
                        "db": {"host": "${HOST}"},
                        "nested": [
                            "${TOKEN}",
                            {"value": "x-${HOST}-y"},
                        ],
                    }
                )
            )
            self.assertEqual(
                load_config(
                    root / "app.json", {"HOST": "prod", "TOKEN": "secret"}
                ),
                {
                    "db": {
                        "host": "prod",
                        "port": 5432,
                        "password": "prefix-secret",
                    },
                    "tags": ["base"],
                    "nested": ["secret", {"value": "x-prod-y"}],
                },
            )

    def test_validation_and_cycle_errors(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            cases = {
                "bad.json": "{",
                "array.json": "[]",
                "include.json": '{"include": [1]}',
                "env.json": '{"value": "${ABSENT}"}',
            }
            for name, content in cases.items():
                (root / name).write_text(content)
                with self.subTest(name=name), self.assertRaises(ValueError):
                    load_config(root / name, {})

            (root / "a.json").write_text('{"include": "b.json"}')
            (root / "b.json").write_text('{"include": "a.json"}')
            with self.assertRaisesRegex(ValueError, "a.json.*b.json.*a.json"):
                load_config(root / "a.json", {})


unittest.main(argv=[sys.argv[0]])
