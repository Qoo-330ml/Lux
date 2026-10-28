"""Unit tests for scripts/check-migration-parity.py (W6)."""

from __future__ import annotations

import importlib.util
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "check-migration-parity.py"


def load_module():
    spec = importlib.util.spec_from_file_location("migration_parity", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class MigrationParityTests(unittest.TestCase):
    def setUp(self) -> None:
        self.module = load_module()

    def test_pairs_shared_names_and_records_unpaired_ones(self) -> None:
        sqlite = {"alpha": 1, "beta": 2, "gamma": 3}
        postgres = {"alpha": 1, "beta": 2}
        manifest = self.module.build_manifest(sqlite, postgres, {})
        self.assertEqual([entry["name"] for entry in manifest["pairs"]], ["alpha", "beta"])
        self.assertEqual([entry["name"] for entry in manifest["sqlite_only"]], ["gamma"])
        self.assertEqual(manifest["postgres_only"], [])
        self.assertEqual(self.module.verify(sqlite, postgres, manifest), [])

    def test_verify_reports_a_new_unlisted_migration(self) -> None:
        sqlite = {"alpha": 1}
        postgres = {"alpha": 1}
        manifest = self.module.build_manifest(sqlite, postgres, {})
        problems = self.module.verify({"alpha": 1, "beta": 2}, postgres, manifest)
        self.assertTrue(any("beta" in problem for problem in problems), problems)

    def test_verify_reports_a_pair_that_lost_a_side(self) -> None:
        sqlite = {"alpha": 1, "beta": 2}
        postgres = {"alpha": 1, "beta": 2}
        manifest = self.module.build_manifest(sqlite, postgres, {})
        problems = self.module.verify({"alpha": 1}, postgres, manifest)
        self.assertTrue(any("pair" in problem.lower() for problem in problems), problems)

    def test_verify_reports_a_moved_version(self) -> None:
        manifest = self.module.build_manifest({"alpha": 1}, {"alpha": 1}, {})
        problems = self.module.verify({"alpha": 5}, {"alpha": 1}, manifest)
        self.assertTrue(any("SQLite version moved" in problem for problem in problems), problems)

    def test_reasons_for_unpaired_files_survive_a_refresh(self) -> None:
        previous = {
            "sqlite_only": [{"name": "gamma", "sqlite": 3, "reason": "FTS table"}],
            "postgres_only": [],
        }
        manifest = self.module.build_manifest({"alpha": 1, "gamma": 3}, {"alpha": 1}, previous)
        self.assertEqual(manifest["sqlite_only"][0]["reason"], "FTS table")


if __name__ == "__main__":
    unittest.main()
