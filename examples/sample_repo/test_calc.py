import unittest

from calc import average


class AverageTest(unittest.TestCase):
    def test_empty(self):
        self.assertEqual(average([]), 0)

    def test_values(self):
        self.assertEqual(average([2, 4, 6]), 4)


if __name__ == "__main__":
    unittest.main()
