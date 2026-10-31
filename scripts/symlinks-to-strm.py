#!/usr/bin/env python3
"""Rewrite symlinked media files inside a library root as `.strm` files.

Lux never follows symbolic links while discovering media (manifest scan,
reconciliation scan, incremental watch, probe), so a symlinked `.mkv`/`.mp4`
is invisible to a library. `.strm` is the supported indirection: a single line
holding a local absolute path, an HTTP(S) URL, an SMB or an FTP target.

This tool converts symlinked media files into sibling `.strm` files so the
library keeps working without changing Lux itself.

Safety rules:
  * directory symlinks are reported, never walked and never rewritten;
  * a broken or non-regular target is reported and left alone;
  * an existing `.strm` with different content is a conflict unless --force;
  * writes are atomic (temporary file + rename) and idempotent;
  * nothing is deleted unless --remove-links is given, and only after the
    `.strm` write succeeded.

Examples:
  python3 scripts/symlinks-to-strm.py --root /media/movies
  python3 scripts/symlinks-to-strm.py --root /media/movies --apply --remove-links
"""

from __future__ import annotations

import argparse
import os
import stat
import sys
import tempfile
from dataclasses import dataclass, field

MEDIA_EXTENSIONS = ("mkv", "mp4")
STRM_EXTENSION = "strm"


@dataclass
class Plan:
    converted: list[tuple[str, str, str]] = field(default_factory=list)
    unchanged: list[tuple[str, str]] = field(default_factory=list)
    conflicts: list[tuple[str, str, str]] = field(default_factory=list)
    broken: list[tuple[str, str]] = field(default_factory=list)
    directory_links: list[str] = field(default_factory=list)
    unsupported: list[str] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Convert symlinked media files into sibling .strm files.",
    )
    parser.add_argument(
        "--root",
        action="append",
        required=True,
        metavar="PATH",
        help="library root to scan; may be repeated",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="write files; without this flag the run is a dry run",
    )
    parser.add_argument(
        "--remove-links",
        action="store_true",
        help="delete a symlink after its .strm file was written",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="overwrite an existing .strm that points at another target",
    )
    parser.add_argument(
        "--extension",
        action="append",
        default=None,
        metavar="EXT",
        help="media extension to convert (default: mkv, mp4)",
    )
    parser.add_argument(
        "--quiet",
        action="store_true",
        help="only print the summary",
    )
    return parser.parse_args(argv)


def is_media_name(name: str, extensions: tuple[str, ...]) -> bool:
    stem, dot, extension = name.rpartition(".")
    if not dot or not stem:
        return False
    return extension.lower() in extensions


def strm_target_for(link_path: str) -> str:
    target = os.path.realpath(link_path)
    if not os.path.isabs(link_path):
        target = os.path.realpath(os.path.abspath(link_path))
    return target


def strm_contents(target: str) -> str:
    return target + "\n"


def read_text(path: str) -> str | None:
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as handle:
            return handle.read()
    except OSError:
        return None


def write_atomic(path: str, contents: str) -> None:
    directory = os.path.dirname(path) or "."
    descriptor, temporary = tempfile.mkstemp(
        dir=directory, prefix=".strm-", suffix=".tmp"
    )
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(contents)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except BaseException:
        try:
            os.unlink(temporary)
        except OSError:
            pass
        raise


def collect(root: str, extensions: tuple[str, ...], plan: Plan) -> None:
    for directory, subdirectories, filenames in os.walk(root, followlinks=False):
        kept = []
        for name in subdirectories:
            path = os.path.join(directory, name)
            if os.path.islink(path):
                plan.directory_links.append(path)
                continue
            kept.append(name)
        subdirectories[:] = kept

        for name in filenames:
            path = os.path.join(directory, name)
            try:
                mode = os.lstat(path).st_mode
            except OSError as error:
                plan.errors.append(f"{path}: {error}")
                continue
            if not stat.S_ISLNK(mode):
                continue
            if not is_media_name(name, extensions):
                plan.unsupported.append(path)
                continue
            try:
                target_mode = os.stat(path).st_mode
            except OSError as error:
                plan.broken.append((path, str(error)))
                continue
            if not stat.S_ISREG(target_mode):
                plan.broken.append((path, "target is not a regular file"))
                continue
            plan.converted.append((path, strm_target_for(path), name))


def strm_path_for(link_path: str) -> str:
    directory, name = os.path.split(link_path)
    stem = name.rpartition(".")[0]
    return os.path.join(directory, f"{stem}.{STRM_EXTENSION}")


def apply_plan(plan: Plan, apply: bool, force: bool, remove_links: bool) -> Plan:
    result = Plan(
        directory_links=list(plan.directory_links),
        unsupported=list(plan.unsupported),
        broken=list(plan.broken),
        errors=list(plan.errors),
    )
    for link_path, target, _name in plan.converted:
        strm_path = strm_path_for(link_path)
        contents = strm_contents(target)
        existing = read_text(strm_path) if os.path.lexists(strm_path) else None
        if existing is not None:
            if existing.strip() == contents.strip():
                result.unchanged.append((link_path, strm_path))
                if apply and remove_links:
                    try:
                        os.unlink(link_path)
                    except OSError as error:
                        result.errors.append(f"{link_path}: {error}")
                continue
            if not force:
                result.conflicts.append((link_path, strm_path, existing.strip()))
                continue
        if not apply:
            result.converted.append((link_path, target, strm_path))
            continue
        try:
            write_atomic(strm_path, contents)
        except OSError as error:
            result.errors.append(f"{strm_path}: {error}")
            continue
        if remove_links:
            try:
                os.unlink(link_path)
            except OSError as error:
                result.errors.append(f"{link_path}: {error}")
        result.converted.append((link_path, target, strm_path))
    return result


def report(plan: Plan, apply: bool, quiet: bool) -> None:
    verb = "wrote" if apply else "would write"
    if not quiet:
        for link_path, target, strm_path in plan.converted:
            print(f"{verb} {strm_path} -> {target}  (from {link_path})")
        for link_path, strm_path in plan.unchanged:
            print(f"unchanged {strm_path}  (from {link_path})")
        for link_path, strm_path, existing in plan.conflicts:
            print(
                f"conflict {strm_path} points at {existing!r}; "
                f"rerun with --force to replace  (from {link_path})",
                file=sys.stderr,
            )
        for link_path, reason in plan.broken:
            print(f"broken {link_path}: {reason}", file=sys.stderr)
        for path in plan.directory_links:
            print(f"skipped directory symlink {path}", file=sys.stderr)
        for path in plan.unsupported:
            print(f"skipped non-media symlink {path}", file=sys.stderr)
        for message in plan.errors:
            print(f"error {message}", file=sys.stderr)
    print(
        "summary: "
        f"{verb} {len(plan.converted)}, "
        f"unchanged {len(plan.unchanged)}, "
        f"conflict {len(plan.conflicts)}, "
        f"broken {len(plan.broken)}, "
        f"directory-links {len(plan.directory_links)}, "
        f"unsupported {len(plan.unsupported)}, "
        f"errors {len(plan.errors)}"
        + ("" if apply else "  [dry run: pass --apply to write]")
    )


def main(argv: list[str]) -> int:
    arguments = parse_args(argv)
    extensions = tuple(
        extension.lower().lstrip(".")
        for extension in (arguments.extension or MEDIA_EXTENSIONS)
    )
    plan = Plan()
    for root in arguments.root:
        if not os.path.isdir(root):
            print(f"error {root}: not a directory", file=sys.stderr)
            return 2
        collect(os.path.abspath(root), extensions, plan)
    final = apply_plan(plan, arguments.apply, arguments.force, arguments.remove_links)
    report(final, arguments.apply, arguments.quiet)
    return 1 if final.errors else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
