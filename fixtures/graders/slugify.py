import pathlib
import sys
import unittest

repo = pathlib.Path(sys.argv[1])
sys.path.insert(0, str(repo))
from src.tiny_tasks import slugify


class SlugifyGrader(unittest.TestCase):
    def test_spacing_and_punctuation(self):
        self.assertEqual(slugify("  Hello,   world!  "), "hello-world")
        self.assertEqual(slugify("one___two---three"), "one-two-three")

    def test_accents_and_empty(self):
        self.assertEqual(slugify("Crème brûlée"), "creme-brulee")
        self.assertEqual(slugify("--- !!! ---"), "")


unittest.main(argv=[sys.argv[0]])
