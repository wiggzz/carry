import importlib.util
import pathlib
import sys
import unittest

repo = pathlib.Path(sys.argv[1])
sys.path.insert(0, str(repo))
from src.tiny_tasks import clamp


class ClampGrader(unittest.TestCase):
    def test_boundaries_and_outside(self):
        self.assertEqual(clamp(-1, 0, 10), 0)
        self.assertEqual(clamp(11, 0, 10), 10)
        self.assertEqual(clamp(0, 0, 10), 0)
        self.assertEqual(clamp(10, 0, 10), 10)

    def test_float_and_invalid_range(self):
        self.assertEqual(clamp(1.5, 0.25, 2.5), 1.5)
        with self.assertRaises(ValueError):
            clamp(1, 4, 3)


unittest.main(argv=[sys.argv[0]])
