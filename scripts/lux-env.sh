#!/usr/bin/env bash
# Point every project build, test and scratch path at this checkout on the external drive.
#
# Usage:  source scripts/lux-env.sh
#
# The internal SSD is small; writing Rust targets, test fixtures, benchmark work directories or
# browser caches under /private/tmp, /var/folders or ~/Library/Caches has repeatedly frozen this
# machine. This file is the executable form of docs/BUILD-OUTPUT-LOCATIONS.md and is safe to source
# from any shell, script or agent session.
#
# shellcheck shell=bash

LUX_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")/.." && pwd)" || {
  echo "lux-env.sh: cannot resolve the project root" >&2
  exit 1
}
export LUX_ROOT

# Rust build and test artifacts.
export CARGO_TARGET_DIR="${LUX_ROOT}/target"

# POSIX temporary directory: test fixtures, mktemp(1), Chromium profiles, compiler scratch files.
export TMPDIR="${LUX_ROOT}/.tmp"
export TMP="${TMPDIR}"
export TEMP="${TMPDIR}"

# SQLite spills sorts and journals into SQLITE_TMPDIR before falling back to TMPDIR and /var/tmp.
export SQLITE_TMPDIR="${TMPDIR}"

# Caches that follow the XDG base directory layout.
export XDG_CACHE_HOME="${LUX_ROOT}/.cache"

# pnpm global home; the content store itself stays in the tracked-ignored .pnpm-store directory.
export PNPM_HOME="${LUX_ROOT}/.pnpm"

# Playwright browser binaries and download cache (default is ~/Library/Caches/ms-playwright).
export PLAYWRIGHT_BROWSERS_PATH="${LUX_ROOT}/.playwright-browsers"

mkdir -p "${TMPDIR}" "${XDG_CACHE_HOME}" "${PNPM_HOME}" "${PLAYWRIGHT_BROWSERS_PATH}"

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  cat <<EOF
LUX_ROOT=${LUX_ROOT}
CARGO_TARGET_DIR=${CARGO_TARGET_DIR}
TMPDIR=${TMPDIR}
SQLITE_TMPDIR=${SQLITE_TMPDIR}
XDG_CACHE_HOME=${XDG_CACHE_HOME}
PNPM_HOME=${PNPM_HOME}
PLAYWRIGHT_BROWSERS_PATH=${PLAYWRIGHT_BROWSERS_PATH}

These variables are exported only inside the shell that sources this file:
  source scripts/lux-env.sh
EOF
fi
