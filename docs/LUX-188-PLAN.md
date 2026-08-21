# LUX-188：可恢复的人物索引重建任务

## Objective

将当前启动时一次性执行的人员出演关系重建改为按媒体库持久化的后台任务，降低大库 `OFFSET` 扫描、
进程重启和重复重建带来的成本，同时保持前台人物查询继续使用已有 `person_credits` 索引。

## Contract

- 任务状态：`QUEUED`、`RUNNING`、`COMPLETED`、`CANCELLED`、`FAILED`。
- 任务按媒体库保持一个当前记录；同一媒体库只能有一个有效 worker。
- worker 领取时生成不可复用的 `run_token`。所有进度和终态更新必须匹配任务 ID 与 token。
- 条目按 `media_items.id` keyset 分页，过滤可见的电影、剧集、季和集。
- 条目状态只在成功替换关系事务中写入；来源指纹为空时永远不跳过。
- 管理 API 使用统一 `{ error: { code, message, requestId } }` 错误合同。

## Implementation slices

1. Migrations and storage contracts
   - SQLite/PostgreSQL task and per-item state tables.
   - Keyset listing, atomic claim, progress, cancel, requeue and fingerprint methods.
   - Query indexes verified against the existing library/type/visible index.
2. Rebuild worker
   - Startup recovery of abandoned workers.
   - Bounded batches, cancellation checks, token-guarded state updates.
   - Missing relation and no-fingerprint behavior.
3. Admin API
   - Paginated status, requeue and cancellation endpoints.
   - Admin authentication and CSRF/API-key behavior.

## Files

- `docs/LUX-DEVELOPMENT.md`
- `docs/LUX-188-PLAN.md`
- `migrations/0078_person_index_rebuild_jobs.sql`
- `migrations/0079_person_index_item_state.sql`
- `migrations/0080_person_index_query_indexes.sql`
- `migrations-postgres/0078_person_index_rebuild_jobs.sql`
- `migrations-postgres/0079_person_index_item_state.sql`
- `migrations-postgres/0080_person_index_query_indexes.sql`
- `src/storage/mod.rs`
- `src/application/people.rs`
- `src/api/mod.rs`
- `tests/people_api.rs`

## Verification

```bash
cargo test --locked --test people_api --lib storage
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
uname -m
```

当前记录（2026-08-21）：SQLite 空库迁移、人物索引任务专项测试和 ARM 本机检查已完成；本机
PostgreSQL daemon 不可用，因此 PostgreSQL 空库迁移保留为未实测，不将其标记为通过。

## Boundaries

- Do not change the existing person resource layout or DTO contract.
- Do not run full-library work in HTTP handlers.
- Do not treat a missing source fingerprint as unchanged.
- Do not allow a stale worker to update a newly requeued task.
- Do not modify unrelated dirty worktree files.
