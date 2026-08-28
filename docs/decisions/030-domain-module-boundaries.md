# ADR-030: Keep large Rust surfaces behind domain module boundaries

## Status

Accepted

## Date

2026-08-28

## Context

Lux remains a modular monolith, but three Rust modules have grown beyond 7,000 lines and two
exceed 20,000 lines:

- `src/api/mod.rs` combines route composition, protocol adapters, handlers, and media serving.
- `src/storage/mod.rs` combines database setup, shared records, and all SQL repositories.
- `src/application/people.rs` combines People use cases, relationship persistence, metadata,
  image resources, and index recovery.

The size makes ownership and review difficult and increases the amount of code considered by
incremental compilation. A split must preserve the current HTTP and storage contracts. The
project must remain one Rust process with the existing SQLite/PostgreSQL abstraction, and this
maintenance task must not change public models, migrations, or dependencies.

## Decision

Use thin facade modules with explicit domain children:

- `api` keeps `AppState`, shared middleware/error helpers, route composition, and stable re-exports;
  domain children own admin, Emby, media, playback, and user handlers.
- `storage` keeps `Database`, shared records, backend helpers, and stable re-exports;
  domain children own catalog, jobs, library, media, metadata, notifications, people, sessions,
  and users repositories. The Emby migration repository remains an explicitly named compatibility
  child alongside them.
- `application::people` keeps the public People types and service construction;
  child modules own relationship/matching, metadata, assets, and index-rebuild/recovery logic.

Children may use private shared helpers through explicit `pub(super)` or `pub(crate)` boundaries.
No `include!`-based textual partitioning is used: each child is a real Rust module with a named
responsibility. The migration is purely organizational, so route paths, DTO serialization, error
codes, SQL statements, schema migrations, and runtime configuration remain unchanged.

## Alternatives considered

### Leave the files monolithic

Rejected: it preserves the current ownership ambiguity, makes review and targeted compilation
harder, and encourages adding more unrelated code to the same files.

### Split only by technical layer

Rejected: files such as `handlers.rs` and `queries.rs` would still mix unrelated product domains;
domain ownership makes dependencies and future task scope clearer.

### Use `include!` to move text without Rust module boundaries

Rejected: it changes file layout without improving visibility, dependency, or compile boundaries,
and makes tooling and diagnostics harder to understand.

### Extract microservices

Rejected: ADR-001 and the product specification require a single-process modular monolith for the
first release; deployment and transaction costs would be disproportionate to this maintenance goal.

## Consequences

- New work has a clear destination and smaller review units.
- Existing callers continue using the facade paths, so this refactor does not require API or
  database migration compatibility work.
- Some shared implementation helpers need explicit visibility declarations between sibling modules.
- The repository facade intentionally retains shared storage models and backend-specific helpers;
  domain children contain the Database methods and the facade contains no duplicate repository
  implementation.
- File boundaries alone do not guarantee faster builds; compile-time impact must be measured after
  the split rather than assumed.
