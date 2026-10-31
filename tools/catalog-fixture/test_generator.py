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

class SpecTargetSeedTests(unittest.TestCase):
    DEFAULTS = {
        "movies": 8,
        "series_count": 2,
        "episodes_per_series": 6,
        "seasons_per_series": 2,
        "detail_items": 3,
        "user_states": 5,
        "actor_items": 2,
        "actors_per_item": 3,
        "people_count": 5,
        "poster_limit": 2,
        "session_token": "fixed-token",
        "now": 1_700_000_000,
    }

    def seed(self, directory: pathlib.Path, name: str = "lux.db", **overrides):
        root = directory / name
        root.mkdir()
        database = root / "catalog.db"
        migrated_database(database)
        kwargs = dict(self.DEFAULTS)
        kwargs.update(overrides)
        manifest = fixture.seed_spec_target(database, root / "media", **kwargs)
        return manifest, database

    def test_spec_target_can_seed_nfo_payloads_for_the_current_schema(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest, database = self.seed(
                pathlib.Path(directory), nfo_items=3, nfo_bytes=512
            )
            self.assertEqual(manifest["nfoItemCount"], 3)
            connection = sqlite3.connect(database)
            try:
                has_cache = (
                    connection.execute(
                        "SELECT 1 FROM sqlite_master WHERE type='table' "
                        "AND name='media_item_nfo_cache'"
                    ).fetchone()
                    is not None
                )
                inline = connection.execute(
                    "SELECT length(nfo_metadata_json) FROM media_items "
                    "WHERE item_type = 'MOVIE' ORDER BY title"
                ).fetchall()
                if has_cache:
                    cached = connection.execute(
                        "SELECT length(cache.json), length(cache.fingerprint) "
                        "FROM media_item_nfo_cache cache "
                        "JOIN media_items item ON item.id = cache.item_id "
                        "WHERE item.item_type = 'MOVIE' ORDER BY item.title"
                    ).fetchall()
                else:
                    cached = connection.execute(
                        "SELECT length(nfo_metadata_json), "
                        "length(nfo_metadata_fingerprint) FROM media_items "
                        "WHERE item_type = 'MOVIE' ORDER BY title"
                    ).fetchall()
            finally:
                connection.close()
            self.assertEqual(len(inline), 8)
            self.assertEqual(len(cached), 3)
            for json_bytes, fingerprint_bytes in cached:
                self.assertGreaterEqual(json_bytes, 400)
                self.assertLessEqual(json_bytes, 512 + 128)
                self.assertEqual(fingerprint_bytes, 32)
            if has_cache:
                self.assertTrue(all(row[0] is None for row in inline))
            else:
                self.assertTrue(all(row[0] is None for row in inline[3:]))

    def test_spec_target_seed_builds_hierarchy_states_and_credits(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest, database = self.seed(pathlib.Path(directory))
            self.assertEqual(manifest["movieCount"], 8)
            self.assertEqual(manifest["seriesCount"], 2)
            self.assertEqual(manifest["seasonCount"], 4)
            self.assertEqual(manifest["episodeCount"], 12)
            self.assertEqual(manifest["itemCount"], 26)
            self.assertEqual(manifest["imageCount"], 52)
            self.assertEqual(manifest["sourceCount"], manifest["itemCount"])
            self.assertEqual(manifest["streamCount"], 9)
            self.assertEqual(manifest["userStateCount"], 5)
            self.assertEqual(manifest["creditCount"], 6)
            self.assertEqual(len(manifest["contentSha256"]), 64)

            connection = sqlite3.connect(database)
            try:
                self.assertEqual(
                    connection.execute(
                        "SELECT COUNT(*) FROM media_items WHERE item_type = 'EPISODE'"
                        " AND series_id IS NOT NULL AND parent_id IS NOT NULL"
                    ).fetchone()[0],
                    12,
                )
                self.assertEqual(
                    connection.execute(
                        "SELECT COUNT(*) FROM media_items WHERE item_type IN ('MOVIE', 'SERIES')"
                        " AND parent_id IS NOT NULL"
                    ).fetchone()[0],
                    0,
                )
                self.assertEqual(
                    connection.execute(
                        "SELECT COUNT(*) FROM media_items s JOIN media_items e"
                        " ON e.parent_id = s.id"
                        " WHERE s.item_type = 'SEASON' AND e.series_id = s.series_id"
                    ).fetchone()[0],
                    12,
                )
                self.assertEqual(
                    connection.execute("SELECT COUNT(*) FROM user_item_state").fetchone()[0],
                    5,
                )
                self.assertEqual(
                    connection.execute(
                        "SELECT COUNT(*) FROM user_item_state WHERE position_ticks > 0"
                    ).fetchone()[0],
                    5,
                )
                self.assertEqual(
                    connection.execute("SELECT COUNT(*) FROM person_credits").fetchone()[0], 6
                )
                self.assertEqual(
                    connection.execute(
                        "SELECT COUNT(*) FROM person_credits WHERE lux_person_id IS NOT NULL"
                    ).fetchone()[0],
                    6,
                )
                self.assertEqual(
                    connection.execute(
                        "SELECT COUNT(*) FROM item_images i"
                        " LEFT JOIN media_items m ON m.id = i.item_id WHERE m.id IS NULL"
                    ).fetchone()[0],
                    0,
                )
                self.assertEqual(
                    connection.execute(
                        "SELECT COUNT(*) FROM media_sources s"
                        " LEFT JOIN filesystem_entries f ON f.id = s.filesystem_entry_id"
                        " WHERE f.id IS NULL"
                    ).fetchone()[0],
                    0,
                )
            finally:
                connection.close()

    def test_spec_target_seed_is_deterministic(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            first, first_db = self.seed(root, "first")
            second, second_db = self.seed(root, "second")
            self.assertEqual(first["contentSha256"], second["contentSha256"])
            self.assertEqual(
                {key: first[key] for key in first if key.endswith("Count")},
                {key: second[key] for key in second if key.endswith("Count")},
            )

            def item_ids(database):
                connection = sqlite3.connect(database)
                try:
                    return connection.execute(
                        "SELECT item_type, id FROM media_items ORDER BY item_type, id"
                    ).fetchall()
                finally:
                    connection.close()

            self.assertEqual(item_ids(first_db), item_ids(second_db))

    def test_spec_target_validates_counts(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            database = root / "catalog.db"
            migrated_database(database)
            with self.assertRaises(ValueError):
                fixture.seed_spec_target(database, root / "media", movies=0, series_count=0)
            with self.assertRaises(ValueError):
                fixture.seed_spec_target(
                    database, root / "media", series_count=1, episodes_per_series=2, seasons_per_series=3
                )


if __name__ == "__main__":
    unittest.main()
