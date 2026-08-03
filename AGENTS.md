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
4. Keep increments small, compilable, and independently revertible. Run the relevant checks after
   every increment and commit each completed increment atomically.
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

## Security and reliability rules

- Never commit passwords, user tokens, cookies, real `.strm` URLs, or user data. The project owner has explicitly approved the fixed third-party TMDb fallback key embedded in the built-in TMDb implementation; it must never be returned by an API or written to logs.
- Never log credentials, access tokens, cookies, full query strings, or complete external URLs.
- Validate every external input, canonicalize media paths, and enforce the configured library root.
- Do not trust forwarded headers unless the request source is in the configured trusted proxy range.
- Do not use `unwrap`, `expect`, or `panic` in production paths without a task-specific justification.
- Do not execute blocking filesystem or process work on Tokio core workers.
- Do not scan a whole library, call TMDb, parse NFO, or run `ffprobe` in a user request.
- Do not add transcoding, proxying for `.strm`, public registration, backup/restore, or unrelated
  Emby endpoints outside the assigned task.

## Required checks

Run checks relevant to the changed surface. The baseline Rust checks are:

```bash
cargo build --locked
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
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
