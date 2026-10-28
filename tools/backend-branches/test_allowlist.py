"""Unit tests for scripts/check-backend-branches.py (W6)."""

from __future__ import annotations

import importlib.util
import pathlib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "check-backend-branches.py"


def load_module():
    spec = importlib.util.spec_from_file_location("backend_branches", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class BackendBranchTests(unittest.TestCase):
    def setUp(self) -> None:
        self.module = load_module()

    def test_manifest_records_counts_and_notes(self) -> None:
        manifest = self.module.build_manifest({"src/a.rs": 2}, {})
        self.assertEqual(manifest["files"][0]["path"], "src/a.rs")
        self.assertEqual(manifest["files"][0]["count"], 2)
        self.assertTrue(manifest["files"][0]["note"])
        self.assertEqual(manifest["total"], 2)

    def test_unregistered_file_is_a_problem(self) -> None:
        manifest = self.module.build_manifest({"src/a.rs": 1}, {})
        problems, improvements = self.module.verify({"src/a.rs": 1, "src/b.rs": 1}, manifest)
        self.assertTrue(any("src/b.rs" in problem for problem in problems), problems)
        self.assertEqual(improvements, [])

    def test_growth_in_a_registered_file_is_a_problem(self) -> None:
        manifest = self.module.build_manifest({"src/a.rs": 1}, {})
        problems, _ = self.module.verify({"src/a.rs": 3}, manifest)
        self.assertTrue(any("grew 1 -> 3" in problem for problem in problems), problems)

    def test_reduction_is_reported_but_not_a_failure(self) -> None:
        manifest = self.module.build_manifest({"src/a.rs": 3}, {})
        problems, improvements = self.module.verify({"src/a.rs": 1}, manifest)
        self.assertEqual(problems, [])
        self.assertTrue(any("reduced 3 -> 1" in item for item in improvements), improvements)

    def test_disappeared_file_is_a_problem(self) -> None:
        manifest = self.module.build_manifest({"src/a.rs": 1}, {})
        problems, _ = self.module.verify({}, manifest)
        self.assertTrue(any("registered but has no backend branches" in p for p in problems), problems)

    def test_existing_note_survives_a_refresh(self) -> None:
        previous = {"files": [{"path": "src/a.rs", "count": 1, "note": "dialect template"}]}
        manifest = self.module.build_manifest({"src/a.rs": 2}, previous)
        self.assertEqual(manifest["files"][0]["note"], "dialect template")


if __name__ == "__main__":
    unittest.main()
