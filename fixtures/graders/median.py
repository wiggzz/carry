import pathlib
import sys
import unittest

repo = pathlib.Path(sys.argv[1])
sys.path.insert(0, str(repo))
from src.tiny_tasks import median


class MedianGrader(unittest.TestCase):
    def test_even_and_odd(self):
        self.assertEqual(median([9, 1, 4]), 4)
        self.assertEqual(median([4, 1, 3, 2]), 2.5)

    def test_input_is_unchanged_and_empty_fails(self):
        values = [3, 1, 2]
        median(values)
        self.assertEqual(values, [3, 1, 2])
        with self.assertRaises(ValueError):
            median([])


unittest.main(argv=[sys.argv[0]])
