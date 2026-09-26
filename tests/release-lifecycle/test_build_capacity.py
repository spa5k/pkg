#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import unittest

SPEC = importlib.util.spec_from_file_location('capacity', Path(__file__).with_name('wait_for_build_capacity.py'))
CAPACITY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CAPACITY)


class BuildCapacityTests(unittest.TestCase):
    def run_wait(self, loads, timeout=600):
        samples = iter(loads)
        clock = [0]
        records = []
        def advance(seconds):
            clock[0] += seconds
        CAPACITY.wait_for_capacity(3, lambda: next(samples), lambda: clock[0], advance,
                                   records.append, timeout)
        return clock[0], records

    def test_transient_idle_sample_does_not_admit_busy_runner(self):
        seconds, rows = self.run_wait([10.88, 2, 9, 3, 2, 1])
        self.assertEqual(seconds, 75)
        self.assertEqual(len(rows), 6)

    def test_nonfinite_or_negative_load_does_not_admit(self):
        for load in [float('nan'), float('inf'), -1]:
            with self.subTest(load=load), self.assertRaises(ValueError):
                self.run_wait([load], timeout=30)

    def test_deadline_is_bounded(self):
        with self.assertRaises(TimeoutError):
            self.run_wait([8, 8, 8], timeout=20)


if __name__ == '__main__':
    unittest.main()
