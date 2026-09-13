#!/usr/bin/env python3
"""Regression coverage for canonical FrontierHarness attempt selection."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest


PATCHER = Path(__file__).with_name("patch_calculate_cost.py")
spec = importlib.util.spec_from_file_location("patch_calculate_cost", PATCHER)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class CalculateCostPatchTests(unittest.TestCase):
    def test_keeps_the_canonical_attempt_when_carry_has_nested_result_json(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            target = Path(raw) / "calculate-cost.py"
            target.write_text("before\n" + module.OLD + "after\n")
            module.apply(target)
            patched = target.read_text()
            self.assertIn('leaves = [p for p in paths if (p.parent / "agent").is_dir()]', patched)
            self.assertNotIn('not any(p.parent in q.parents', patched)
            module.apply(target)
            self.assertEqual(target.read_text(), patched)

    def test_refuses_an_unknown_upstream_layout(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            target = Path(raw) / "calculate-cost.py"
            target.write_text("unexpected\n")
            with self.assertRaisesRegex(RuntimeError, "calculate-cost layout"):
                module.apply(target)


if __name__ == "__main__":
    unittest.main()
