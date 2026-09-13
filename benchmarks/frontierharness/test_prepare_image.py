#!/usr/bin/env python3
"""Regression coverage for the pinned FrontierHarness image-prepull repair."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest


PATCHER = Path(__file__).with_name("patch_prepare_image.py")
spec = importlib.util.spec_from_file_location("patch_prepare_image", PATCHER)
assert spec and spec.loader
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class PrepareImagePatchTests(unittest.TestCase):
    def test_datacurve_image_is_pulled_as_an_argv_element(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            target = Path(raw) / "run-trials.sh"
            target.write_text("before\n" + module.OLD + "after\n")
            module.apply(target)
            patched = target.read_text()
            self.assertIn('retry_transport runta exec "$runtime" -- timeout 1800 docker pull "$image"', patched)
            self.assertIn('retry_transport runta exec "$runtime" -- timeout 1800 docker pull ubuntu:24.04', patched)
            self.assertNotIn('sh -lc "timeout 1800 docker pull', patched)
            self.assertIn('toml="$TASKS/$task/task.toml"', patched)
            module.apply(target)
            self.assertEqual(target.read_text(), patched)

    def test_refuses_an_unknown_upstream_layout(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            target = Path(raw) / "run-trials.sh"
            target.write_text("unexpected\n")
            with self.assertRaisesRegex(RuntimeError, "prepare_image layout"):
                module.apply(target)


if __name__ == "__main__":
    unittest.main()
