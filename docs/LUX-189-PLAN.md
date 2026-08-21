# LUX-189：后台任务资源隔离与管理员任务体验

## Objective

吸收 PR #14 中与 Lux 当前架构一致的工程改进：让 watcher 初始化不依赖可能饱和的 Tokio
blocking pool，限制整库元数据任务的总资源占用，使任务在重启后可恢复，并让管理员任务页面的
加载和 SSE 刷新行为稳定、可诊断。

## Contract

- watcher 的同步注册工作不得在 Tokio 核心 worker 上执行，也不得创建无界初始化线程；初始化失败
  只影响对应媒体库根路径，并保留明确日志。
- metadata 任务在单进程内具有有界的全局 worker 数量；同一任务同一时间只能有一个 owner。
- 进程重启后遗留的 `RUNNING` 条目重新进入 `PENDING`，不能被错误计为基础设施失败。
- metadata 进度事件按任务节流，完成、失败和取消事件立即发布；同一前端查询不因两个作用域重复失效。
- 管理操作页面加载态提供可见反馈和可访问状态；错误态不显示持续 spinner。
- 任务摘要按持久化的任务范围和媒体库身份查询，不在列表行中重复扫描整张明细表。
- 外部图片的瞬时失败只做有限退避重试，不重试永久错误，不泄露完整 URL 或凭据。

## Implementation slices

1. Watcher bootstrap
   - bounded dedicated initialization threads;
   - executor/thread-affinity regression test;
   - preserve watcher cancellation and root reconciliation.
2. Admin task experience
   - visible loading state and `aria-busy` semantics;
   - throttled metadata job events and query invalidation tests.
3. Metadata job reliability
   - persisted library/scope summary fields and indexes;
   - owner guard, bounded global permits, restart requeue and safe error details;
   - migration and storage/reidentify integration tests.
4. Documentation and verification
   - ADR for blocking-work isolation and metadata event semantics;
   - update LUX development acceptance criteria;
   - Rust/Web checks and ARM architecture record.

## Files

- `src/application/watch.rs`
- `src/application/reidentify.rs`
- `src/application/images.rs`
- `src/storage/mod.rs`
- `migrations/`
- `migrations-postgres/`
- `web/src/features/admin/AdminOperationsPage.tsx`
- `web/src/features/admin/useAdminEvents.ts`
- `web/src/react.css`
- `tests/watch.rs`
- `tests/reidentify.rs`
- `tests/storage.rs`
- `web/tests/`
- `docs/decisions/`
- `docs/LUX-DEVELOPMENT.md`

## Boundaries

- Do not move blocking work into async tasks merely to avoid a busy blocking pool.
- Do not add an unbounded thread per library root or an unbounded metadata queue.
- Do not use an in-memory owner guard as a substitute for a database state transition.
- Do not change Emby person DTOs, media resource layouts, or playback behavior.
- Do not claim NAS/x86 performance from the local ARM machine.

## Verification

```bash
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
pnpm --dir web install --frozen-lockfile
pnpm --dir web test
pnpm --dir web build
uname -m
```
