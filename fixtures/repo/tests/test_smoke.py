import unittest

from src.tiny_tasks import clamp, median, slugify


class SmokeTests(unittest.TestCase):
    def test_clamp_inside_range(self):
        self.assertEqual(clamp(4, 1, 9), 4)

    def test_slugify_words(self):
        self.assertEqual(slugify("Carry Agent"), "carry-agent")

    def test_median_odd(self):
        self.assertEqual(median([3, 1, 2]), 2)


if __name__ == "__main__":
    unittest.main()
