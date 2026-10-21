import hashlib
import importlib.util
import json
import pathlib
import sqlite3
import tempfile
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("generate.py")
SPEC = importlib.util.spec_from_file_location("lux_catalog_fixture", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
fixture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(fixture)
REPOSITORY_ROOT = pathlib.Path(__file__).resolve().parents[2]


def migrated_database(path: pathlib.Path) -> None:
    connection = sqlite3.connect(path)
    try:
        for migration in sorted((REPOSITORY_ROOT / "migrations").glob("*.sql")):
            connection.executescript(migration.read_text(encoding="utf-8"))
        connection.commit()
    finally:
        connection.close()


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


class CatalogSeedTests(unittest.TestCase):
    def test_catalog_seed_is_complete_and_repeatable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            database = root / "lux.db"
            migrated_database(database)
            manifest = fixture.seed_catalog(
                database,
                root / "media",
                item_count=30,
                library_count=3,
                poster_limit=4,
                session_token="fixed-token",
                now=1_700_000_000,
            )

            connection = sqlite3.connect(database)
            try:
                self.assertEqual(
                    connection.execute(
                        "SELECT COUNT(*) FROM libraries WHERE system_kind IS NULL"
                    ).fetchone()[0],
                    3,
                )
                self.assertEqual(
                    connection.execute("SELECT COUNT(*) FROM media_items").fetchone()[0], 30
                )
                self.assertEqual(
                    connection.execute("SELECT COUNT(*) FROM media_sources").fetchone()[0],
                    manifest["sourceCount"],
                )
                self.assertEqual(
                    connection.execute("SELECT COUNT(*) FROM media_streams").fetchone()[0],
                    manifest["streamCount"],
                )
                self.assertEqual(
                    connection.execute("SELECT COUNT(*) FROM item_images").fetchone()[0],
                    manifest["imageCount"],
                )
                self.assertEqual(manifest["itemCount"], 30)
                self.assertEqual(len(manifest["libraries"]), 3)
                self.assertEqual(manifest["sessionToken"], "fixed-token")

                session = connection.execute(
                    "SELECT session_token_hash, expires_at FROM web_sessions"
                ).fetchone()
                self.assertEqual(
                    bytes(session[0]), hashlib.sha256(b"fixed-token").digest()
                )
                self.assertEqual(session[1], 1_700_000_000 + 7 * 24 * 60 * 60)

                images = connection.execute(
                    "SELECT image_type, local_path, width, height, content_tag FROM item_images"
                ).fetchall()
                self.assertTrue(images)
                self.assertTrue(all(image[4] and len(image[4]) == 64 for image in images))
                for image_type, local_path, width, height, _tag in images:
                    self.assertIn(image_type, {"POSTER", "FANART", "THUMB"})
                    self.assertTrue(pathlib.Path(local_path).is_file())
                    self.assertGreater(width, 0)
                    self.assertGreater(height, 0)
                    self.assertTrue(
                        pathlib.Path(local_path).is_relative_to(
                            (root / "media").resolve() / "posters"
                        )
                    )

                mixed = connection.execute(
                    "SELECT COUNT(DISTINCT item_type) FROM media_items"
                ).fetchone()[0]
                self.assertEqual(mixed, 2)
            finally:
                connection.close()

            with self.assertRaises(ValueError):
                fixture.seed_catalog(
                    database,
                    root / "media",
                    item_count=1,
                    library_count=1,
                    poster_limit=1,
                )

    def test_small_flushes_keep_foreign_key_order(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            database = root / "lux.db"
            migrated_database(database)
            manifest = fixture.seed_catalog(
                database,
                root / "media",
                item_count=40,
                library_count=3,
                poster_limit=2,
                session_token="batch-token",
                batch_size=5,
            )
            connection = sqlite3.connect(database)
            try:
                self.assertEqual(
                    connection.execute("SELECT COUNT(*) FROM media_items").fetchone()[0], 40
                )
                self.assertEqual(
                    connection.execute("SELECT COUNT(*) FROM item_images").fetchone()[0],
                    manifest["imageCount"],
                )
                self.assertEqual(
                    connection.execute("SELECT COUNT(*) FROM media_streams").fetchone()[0],
                    manifest["streamCount"],
                )
                orphan_images = connection.execute(
                    "SELECT COUNT(*) FROM item_images ii LEFT JOIN media_items mi ON mi.id = ii.item_id WHERE mi.id IS NULL"
                ).fetchone()[0]
                self.assertEqual(orphan_images, 0)
            finally:
                connection.close()

    def test_missing_schema_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            database = pathlib.Path(directory) / "empty.db"
            sqlite3.connect(database).close()
            with self.assertRaises(ValueError):
                fixture.seed_catalog(database, pathlib.Path(directory) / "media", item_count=1)

    def test_placeholder_posters_and_dimensions(self):
        with tempfile.TemporaryDirectory() as directory:
            posters = fixture.poster_pool(pathlib.Path(directory), limit=3)
            self.assertEqual(len(posters), 3)
            self.assertEqual(
                (posters[0]["width"], posters[0]["height"]), (960, 1440)
            )
            self.assertEqual(len(str(posters[0]["content_tag"])), 64)
            self.assertTrue(all(pathlib.Path(poster["path"]).is_file() for poster in posters))

    def test_real_posters_are_reused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            sources = root / "sources"
            sources.mkdir()
            for index in range(2):
                fixture._write_placeholder_png(sources / f"source-{index}.png", index, 12, 18)
            posters = fixture.poster_pool(
                root / "pool", sources, limit=4, placeholder_dimensions=(12, 18)
            )
            self.assertEqual(len(posters), 4)
            self.assertEqual((posters[0]["width"], posters[0]["height"]), (12, 18))
            self.assertEqual(posters[0]["content_tag"], fixture._sha256(sources / "source-0.png"))


if __name__ == "__main__":
    unittest.main()
