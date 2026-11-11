import importlib.util
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location("profile_oracle", Path(__file__).with_name("profile-oracle.py"))
oracle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(oracle)


class KnownProfileBytes(unittest.TestCase):
    def test_context_absolute_and_relative_timestamps(self):
        raw = bytes.fromhex("01032a00070000000902036400000000000000"
                            "03002b0007"
                            "04012b000101"
                            "02042a00080000002c01040502")
        rows = oracle.inner_events(raw, 123, 17, 5)
        self.assertEqual([(row["type"], row["event"], row["thread"], row["context"],
                           row["priority"], row["cpu"], row["tick"], row["capture_offset"])
                          for row in rows],
                         [(1, 42, 7, 9, 2, 3, 100, 123),
                          (3, 43, 7, 9, 2, 3, 107, 142),
                          (4, 43, 7, 9, 2, 3, 364, 147),
                          (2, 42, 8, 300, 4, 5, 366, 153)])

    def test_payload_boundaries_reset_compression_state(self):
        absolute = bytes.fromhex("01032a00070000000902036400000000000000")
        first = oracle.inner_events(absolute + bytes.fromhex("04002a0005"), 0, 17, 5)
        second = oracle.inner_events(absolute, 0, 17, 6)
        self.assertEqual([row["tick"] for row in first], [100, 105])
        self.assertEqual(second[0]["tick"], 100)
        self.assertEqual(oracle.inner_events(b"", 0, 17, 7), [])
        with self.assertRaises(ValueError):
            oracle.inner_events(bytes.fromhex("04002a0005"), 0, 17, 7)

    def test_truncation_unknown_event_and_unknown_flags(self):
        for raw in (bytes.fromhex("01032a0007"), bytes.fromhex("ff002a00"), bytes.fromhex("01102a00")):
            with self.assertRaises(ValueError):
                oracle.inner_events(raw, 0, 17, 5)


if __name__ == "__main__":
    unittest.main()
