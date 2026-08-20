# LUX-187：全局扫描活动与首页即时刷新

## 目标

学习 Plex 的扫描反馈体验：管理员在 Lux 任意页面都能从右上角看到当前扫描任务，
包括媒体库、阶段、进度和当前处理条目的安全显示名；扫描提交后的索引变化通过同源
SSE 通知普通用户和管理员，使首页及媒体库浏览在写入完成后立即刷新。

## 契约

- 管理员活动摘要复用 `GET /api/v1/admin/jobs?status=PENDING` 和
  `GET /api/v1/admin/jobs?status=RUNNING`，响应中的 `currentItem` 只允许是相对媒体
  路径的 basename 或目录相对名，不返回媒体库根路径、完整本地路径、`.strm` 原始目标
  或查询参数。
- 扫描任务状态包含 `scanPhase`：`DISCOVERY`、`INDEXING`、`FINALIZING` 或 `IDLE`。
- `GET /api/v1/events` 只允许已登录的 Lux Web 用户，响应只发送 `ready` 和
  `invalidate` 事件，不携带任务、路径或媒体详情。
- 普通用户事件的 `invalidate` scope 固定为 `home`；管理员事件继续使用原有管理作用域。
- 扫描工作项写入、扫描完成、取消和失败都会发布事件。首页和媒体库查询由前端收到
  `home` 事件后失效并重新读取，保留 15 秒轮询作为断线兜底。
- 右上角入口仅对 `canManageServer` 用户显示；活动浮层支持查看任务与日志、取消任务，
  不在浮层中执行新的扫描。

## 实施切片

1. 扩展扫描任务持久状态和事件端点，补集成测试。
2. 增加普通用户事件监听和 React Query 作用域失效。
3. 增加右上角扫描活动入口、进度和响应式样式。
4. 运行 Rust/Web 检查，记录 ARM 本机架构。

## 验收

- [x] 管理员能在任意已登录 Web 页面看到活动扫描数量、媒体库、阶段和进度。
- [x] 当前条目展示不包含完整本地路径、`.strm` URL、token 或 query string。
- [x] 扫描完成后，普通用户首页和媒体库缓存立即失效并刷新。
- [x] 未认证用户不能读取普通用户事件流；普通用户不能读取管理员事件流。
- [x] 任务取消、失败、重启恢复仍保持既有状态机行为。
- [x] Rust 集成测试、Web 单测、Web 构建、fmt、clippy 通过。

## 预计文件

- `docs/LUX-DEVELOPMENT.md`
- `docs/LUX-187-PLAN.md`
- `migrations/0071_scan_job_activity.sql`
- `migrations-postgres/0071_scan_job_activity.sql`
- `src/application/admin_events.rs`
- `src/application/scanner.rs`
- `src/storage/mod.rs`
- `src/api/mod.rs`
- `tests/scanning_jobs.rs`
- `tests/admin_events.rs`
- `web/src/components/layout/LuxShell.tsx`
- `web/src/features/activity/ScanActivityPopover.tsx`
- `web/src/features/admin/useAdminEvents.ts`
- `web/src/lib/api/client.ts`
- `web/src/lib/api/types.ts`
- `web/src/lib/api/query-keys.ts`
- `web/src/react.css`
- `web/tests/lux-shell.test.tsx`
