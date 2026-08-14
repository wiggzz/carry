import pathlib
import sys
import unittest

repo = pathlib.Path(sys.argv[1])
sys.path.insert(0, str(repo))
from src.manifest import Package, parse_manifest
from src.planner import build_release_order


class ReleasePlanGrader(unittest.TestCase):
    def test_parser_order_comments_and_whitespace(self):
        parsed = parse_manifest(
            "\n# products\n api-v2 : core_lib, schema2 \ncore_lib:\nschema2:\n"
        )
        self.assertEqual(
            parsed,
            [
                Package("api-v2", ("core_lib", "schema2")),
                Package("core_lib", ()),
                Package("schema2", ()),
            ],
        )

    def test_parser_rejects_bad_input(self):
        for text in [
            "broken",
            "9bad:",
            "one:: two",
            "one: dep, dep",
            "one:\none:",
        ]:
            with self.subTest(text=text), self.assertRaises(ValueError):
                parse_manifest(text)

    def test_stable_dependency_order_and_no_mutation(self):
        packages = [
            Package("web", ("core",)),
            Package("docs", ()),
            Package("core", ("schema",)),
            Package("schema", ()),
            Package("tools", ("schema",)),
        ]
        before = list(packages)
        self.assertEqual(
            build_release_order(packages),
            ["docs", "schema", "core", "tools", "web"],
        )
        self.assertEqual(packages, before)

    def test_unknown_duplicate_and_cycle_errors(self):
        with self.assertRaisesRegex(ValueError, "missing"):
            build_release_order([Package("app", ("missing",))])
        with self.assertRaises(ValueError):
            build_release_order([Package("app", ()), Package("app", ())])
        with self.assertRaisesRegex(ValueError, "first.*second|second.*first"):
            build_release_order(
                [Package("first", ("second",)), Package("second", ("first",))]
            )


unittest.main(argv=[sys.argv[0]])
