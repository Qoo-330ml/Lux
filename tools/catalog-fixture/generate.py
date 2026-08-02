#!/usr/bin/env python3
"""Generate a deterministic local catalog fixture without embedding media data."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path


FIXTURE_VERSION = "lux-catalog-fixture-v1"
DEFAULT_FILE_COUNT = 60_000
DEFAULT_DIRECTORY_COUNT = 600
FILE_CONTENT = b"LUX PERF FIXTURE\n"


def fixture_entries(
    root: Path,
    file_count: int = DEFAULT_FILE_COUNT,
    directory_count: int = DEFAULT_DIRECTORY_COUNT,
):
    if file_count < 1:
        raise ValueError("file_count must be positive")
    if directory_count < 1:
        raise ValueError("directory_count must be positive")

    files_per_directory = (file_count + directory_count - 1) // directory_count
    for index in range(file_count):
        bucket = index // files_per_directory
        year = 2000 + index % 100
        path = root / f"bucket-{bucket:04d}" / f"Fixture.Movie.{index:06d}.{year}.mkv"
        yield path


def generate_fixture(
    root: Path,
    file_count: int = DEFAULT_FILE_COUNT,
    directory_count: int = DEFAULT_DIRECTORY_COUNT,
) -> dict[str, object]:
    root = root.expanduser().resolve()
    root.mkdir(parents=True, exist_ok=True)
    entries = list(fixture_entries(root, file_count, directory_count))
    for path in entries:
        path.parent.mkdir(parents=True, exist_ok=True)
        if not path.exists():
            path.write_bytes(FILE_CONTENT)
        elif path.read_bytes() != FILE_CONTENT:
            raise ValueError(f"existing fixture file has unexpected content: {path}")

    manifest = {
        "fixtureVersion": FIXTURE_VERSION,
        "fileCount": file_count,
        "directoryCount": directory_count,
        "contentSha256": hashlib.sha256(FILE_CONTENT).hexdigest(),
    }
    (root / ".lux-fixture.json").write_text(
        json.dumps(manifest, ensure_ascii=False, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    return manifest


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, help="directory to populate")
    parser.add_argument("--files", type=int, default=DEFAULT_FILE_COUNT)
    parser.add_argument("--directories", type=int, default=DEFAULT_DIRECTORY_COUNT)
    return parser


def main() -> int:
    args = _parser().parse_args()
    manifest = generate_fixture(args.root, args.files, args.directories)
    print(json.dumps(manifest, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
