# Lux agent instructions

## Source of truth

- `docs/LUX-DEVELOPMENT.md` is the product specification, architecture boundary, task list, and
  acceptance criteria.
- Work on one `LUX-*` task at a time and do not implement later tasks early.
- If the specification and existing code disagree, stop and report the exact conflict before
  changing the public model, database relationships, framework, or core dependency set.

## Required workflow

1. Read this file, the global completion standard in `docs/LUX-DEVELOPMENT.md`, and the assigned
   task before editing code.
2. Inspect the current implementation and tests, then write a short task plan with the exact files
   expected to change.
3. Use test-driven development for behavior changes: write a failing test, implement the smallest
   passing change, then refactor only when the tests remain green.
4. Keep increments small, compilable, and independently revertible. Run the narrowest relevant
   checks after every increment and commit each completed increment atomically. For Rust behavior
   changes, run the related integration test group with
   `cargo test --locked --test <group> <module>::` (see `docs/TEST-TARGETS.md` for the former
   per-file target → group mapping), or `cargo test --locked --lib <module-or-test-filter>` for
   library tests. Do not run
   `cargo test --locked --all-targets` during routine increments; reserve it for task completion,
   a documented phase gate, cross-cutting changes, or an explicit project-owner request.
5. Report changed files, acceptance results, test results, and remaining risks at the end of a task.

## Engineering boundaries

- Rust is the core service language. The initial Web decision is React + TypeScript as documented
  by ADR-006; changing it requires an explicit ADR update.
- Keep the first release as one Rust process with SQLite and bounded background workers.
- HTTP handlers parse and validate protocol data, call application services, and map DTOs. They do
  not contain SQL, full-library scans, `ffprobe`, or TMDb calls.
- SQL stays in `storage`; paths use `Path`/`PathBuf`; domain IDs use distinct newtypes.
- SQLite schema changes require migrations that run from an empty database.
- Emby routes/DTOs remain separate from Lux API routes/DTOs and domain types.
- All lists are paginated and have a server-side upper bound.

## Build output location

This checkout lives on the external drive (`/Volumes/Toshiba/mywork/Lux`) with ample free space.
The internal SSD is small, and heavy writes to built-in-disk paths repeatedly froze this machine.

- Write every build artifact, cache, test fixture, benchmark work directory, log and screenshot
  inside this project directory: Rust to `target/`, Web to `web/node_modules/` and `web/dist/`,
  temporary data to `.tmp/`.
- Never use `/tmp`, `/private/tmp`, `/var/tmp`, `/var/folders`, `~/Library/Caches` or `~/.cache`
  as a project build, test, benchmark or scratch location, and never copy the repo or `target/`
  onto the internal disk to compile or compare.
- Before any build or test command, run `source scripts/lux-env.sh`; it points
  `CARGO_TARGET_DIR`, `TMPDIR`, `SQLITE_TMPDIR`, `XDG_CACHE_HOME`, `PNPM_HOME` and
  `PLAYWRIGHT_BROWSERS_PATH` at project directories.
- New scripts create scratch space with
  `mktemp -d "${LUX_ROOT}/.tmp/<purpose>.XXXXXX"`, not bare `mktemp -d`.
- Docker build cache lives in the Docker Desktop VM image on the internal disk; relocate that image
  or prune the builder before and after large builds, and keep build contexts out of `/private/tmp`.
- Delete scratch directories when the task ends and verify with section 6 of
  `docs/BUILD-OUTPUT-LOCATIONS.md` that the internal disk did not grow.

## Security and reliability rules

- Never commit passwords, user tokens, cookies, real `.strm` URLs, or user data. The project owner has explicitly approved the fixed third-party TMDb fallback key embedded in the built-in TMDb implementation; it must never be returned by an API or written to logs.
- Never log credentials, access tokens, cookies, full query strings, or complete external URLs.
- Validate every external input, canonicalize media paths, and enforce the configured library root.
- Lux may use `X-Forwarded-For` for IP/CIDR blacklist decisions only through the access-IP
  resolver: it starts from the connection peer attached by server middleware, consults explicitly
  configured trusted-proxy CIDRs plus the single container default gateway detected from
  `/proc/net/route` when the process runs inside a container, and walks the complete chain from
  right to left. The implicit gateway trust is limited to one host address, never the bridge
  subnet, and is disabled outside containers. Raw forwarded values and untrusted peers must never
  be used for ACL decisions. `X-Forwarded-Proto` remains display/transport metadata only and must
  not be used for authentication, authorization, or ACLs.
- Do not use `unwrap`, `expect`, or `panic` in production paths without a task-specific justification.
- Do not execute blocking filesystem or process work on Tokio core workers.
- Do not scan a whole library, call TMDb, parse NFO, or run `ffprobe` in a user request.
- Do not add transcoding beyond the bounded 0–4 tier Web playback defined by ADR-004/ADR-026,
  proxying for `.strm` (which stays Direct-only), public registration, backup/restore, or unrelated
  Emby endpoints outside the assigned task.

## Required checks

### AI test selection

- Before testing, identify the narrowest Rust target named by the assigned task's acceptance
  criteria or the changed behavior. Integration tests are grouped by domain: select one explicit
  `--test <group>` flag, then narrow it with a module prefix (`--test <group> <module>::`).
  `docs/TEST-TARGETS.md` maps every former per-file target to its group; do not use a broad
  test-name filter as a substitute for selecting the target.
- For a library-only change, prefer `cargo test --locked --lib <module-or-test-filter>`.
- Run `cargo test --locked --all-targets` only at the completion or phase-gate conditions stated
  above. It remains part of the full Rust completion gate and is not waived by targeted tests.

Run checks relevant to the changed surface. The baseline Rust checks are:

```bash
cargo build --locked
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
python3 scripts/check-migration-parity.py
python3 scripts/check-backend-branches.py
```

For Web changes also run:

```bash
pnpm --dir web install --frozen-lockfile
pnpm --dir web test
pnpm --dir web build
```

Use `./scripts/check-all.sh` for the complete project check once LUX-002 makes it available. On
this development machine, record ARM validation using `uname -m`; the expected native target is
`arm64`/`aarch64-apple-darwin`. Do not claim NAS/x86 performance from a local ARM run.

## Stage gates

At the end of each documented phase, run that phase's checks, update compatibility or performance
records as required, and stop for project-owner confirmation before entering the next phase. Never
silently expand a task because a later feature appears convenient.
