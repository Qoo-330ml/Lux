import hashlib
import importlib.util
import json
import pathlib
import tempfile
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("generate.py")
SPEC = importlib.util.spec_from_file_location("lux_catalog_fixture", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
fixture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fixture)


class FixtureGeneratorTests(unittest.TestCase):
    def test_generation_is_deterministic_and_writes_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            first = fixture.generate_fixture(root, file_count=12, directory_count=3)
            paths = sorted(root.glob("bucket-*/*.mkv"))
            first_hashes = [hashlib.sha256(path.read_bytes()).hexdigest() for path in paths]

            second = fixture.generate_fixture(root, file_count=12, directory_count=3)
            second_paths = sorted(root.glob("bucket-*/*.mkv"))

            self.assertEqual(first, second)
            self.assertEqual(paths, second_paths)
            self.assertEqual(first_hashes, [hashlib.sha256(path.read_bytes()).hexdigest() for path in second_paths])
            self.assertEqual(json.loads((root / ".lux-fixture.json").read_text()), first)
            self.assertEqual(paths[0].name, "Fixture.Movie.000000.2000.mkv")

    def test_invalid_sizes_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with self.assertRaises(ValueError):
                fixture.generate_fixture(root, file_count=0)
            with self.assertRaises(ValueError):
                fixture.generate_fixture(root, directory_count=0)


if __name__ == "__main__":
    unittest.main()
