import importlib.util
import sys
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "scripts/reconcile_rocksdb_incremental.py"
SPEC = importlib.util.spec_from_file_location("reconcile_rocksdb_incremental", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class CompareGroupsTest(unittest.TestCase):
    @staticmethod
    def stat(qty):
        return {"qty": qty, "events": 1, "orders": {"order"}}

    def test_uniform_and_unmatched_equal_pg(self):
        key = ("swap", "BTCUSDT", "buy")
        rows = MODULE.compare(
            {key: self.stat(3.0)},
            {key: self.stat(2.0)},
            {key: self.stat(1.0)},
            1e-8,
        )
        self.assertEqual(rows[0]["status"], "MATCH")
        self.assertEqual(rows[0]["local_qty"], 3.0)

    def test_missing_pg_group_is_mismatch(self):
        key = ("spot", "ETHUSDT", "sell")
        rows = MODULE.compare({}, {key: self.stat(0.5)}, {}, 1e-8)
        self.assertEqual(rows[0]["status"], "MISMATCH")
        self.assertEqual(rows[0]["qty_diff"], 0.5)

    def test_epsilon_is_inclusive(self):
        key = ("spot", "SOLUSDT", "buy")
        rows = MODULE.compare(
            {key: self.stat(1.0)},
            {key: self.stat(1.0 + 0.5e-8)},
            {},
            1e-8,
        )
        self.assertEqual(rows[0]["status"], "MATCH")


class TimestampTest(unittest.TestCase):
    def test_rfc3339_preserves_microseconds(self):
        self.assertEqual(
            MODULE.rfc3339_us(1_785_314_400_123_999),
            "2026-07-29T08:40:00.123999Z",
        )


if __name__ == "__main__":
    unittest.main()
