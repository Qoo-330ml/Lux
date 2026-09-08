# LUX-244：任务类型与执行计划聚合

## Objective

将管理员任务配置从“每个媒体库一条注册项”调整为三层模型：任务类型、执行计划、媒体库级运行任务。
同一任务类型可以拥有多个执行计划，每个计划独立维护 Cron、启停状态、资源限制和媒体库范围。
执行计划到点后仍按媒体库创建独立运行任务，并继续使用现有扫描锁、去重、取消和重试语义。

目标是减少任务配置数量和查找成本，同时保留不同媒体库错峰执行的能力。

## Assumptions

1. 现有 `scheduled_task_configs` 和旧的 `/admin/scheduled-tasks` API 仍可能被旧客户端使用，迁移期间保留并作为每个媒体库的有效配置镜像。
2. 任务计划只影响 Lux 后台调度，不改变 Emby 兼容 API、实时文件监听或已有运行任务的数据模型。
3. 全量扫描默认继续使用全局扫描锁容量 1；聚合计划不会导致多个全量扫描同时访问文件系统。
4. 任务类型下同一个媒体库只能属于一个启用或停用的执行计划；计划移动必须是原子操作。
5. 现有不同 Cron 的媒体库不会被强行合并；迁移按原有任务类型、插件来源、计划、启用状态和资源限制分组。
6. 全局插件任务（例如 STRM 媒体信息和弹幕匹配）继续保持一个全局注册计划，其媒体库选择仍由插件配置管理；本任务聚合媒体库作用域的注册任务。

## Contract

### Terminology

- **任务类型**：系统或插件提供的能力，例如 `RECONCILIATION_SCAN`、`METADATA_PARSE`。
- **执行计划**：管理员配置的计划组，包含名称、Cron、目标媒体库和资源限制。
- **运行任务**：执行计划触发后按媒体库创建的实际工作，继续显示在运行记录中。

### Persistence

- 新增 `scheduled_task_plans`，保存任务类型、计划名称、Cron、启停状态、来源和资源限制。
- 新增 `scheduled_task_plan_libraries`，保存计划与媒体库的关联。
- `scheduled_task_configs.plan_id` 保存旧任务配置到执行计划的有效镜像关系。
- 旧配置仍保留，旧 API 和现有后台服务通过镜像读取；执行计划更新必须在同一事务内同步镜像。
- 删除媒体库时级联删除其计划关联；空的非默认计划保留为停用/未配置记录，便于审计，不自动复用。

### Plan rules

- 每种任务类型和来源/插件组合至少有一个默认计划；新媒体库加入匹配的默认计划。
- 创建自定义计划时，选中的媒体库从原计划原子移动到新计划。
- 自定义计划可以删除；删除时计划内媒体库原子回到同任务类型、来源和插件匹配的默认计划，旧媒体库任务配置镜像保留并同步到默认计划。
- 同一媒体库在同一任务类型下只能属于一个计划；服务端返回结构化冲突错误，不允许重复归属。
- 默认计划可以接收媒体库，但不能通过编辑把媒体库变成无计划状态；要错峰应创建或选择其他计划。
- 默认计划和全局插件计划不可删除；全局插件计划由插件启停或卸载流程管理。
- 计划的“立即执行”创建一个批次语义的请求，但运行记录仍是一条媒体库一条，单个失败不阻塞其他媒体库。
- 计划到点时只创建一次本计划的调度请求；已有同计划/同媒体库的活动任务时去重，不重复排队。
- 计划之间共享现有资源边界：实时增量扫描优先，全量扫描默认串行，其他任务继续使用已有全局并发限制。

### HTTP API

- `GET /api/v1/admin/scheduled-task-plans?page=1&pageSize=50&taskType=...`：分页返回执行计划及目标媒体库摘要。
- `POST /api/v1/admin/scheduled-task-plans`：创建计划，输入 `taskType`、`name`、可空 `schedule`、`isEnabled` 和 `libraryIds`。
- `PATCH /api/v1/admin/scheduled-task-plans/{planId}`：更新计划名称、Cron、启停状态和目标媒体库；库归属变化与计划配置同事务提交。
- `DELETE /api/v1/admin/scheduled-task-plans/{planId}`：删除自定义媒体库执行计划；计划内媒体库回到匹配的默认计划，返回 204。默认计划和全局插件计划返回结构化冲突错误。
- `POST /api/v1/admin/scheduled-task-plans/{planId}/run`：立即执行计划，返回计划内已接受的媒体库运行任务 ID。
- 计划 API 仅允许管理员 Web session 和 CSRF；所有列表分页且有服务端上限。
- 现有 `/api/v1/admin/scheduled-tasks` 和其 PUT 接口继续兼容；旧接口修改单库计划时自动形成单库覆盖计划或更新匹配的计划，不破坏旧客户端。

### UI

- “已注册任务”主列表改为按任务类型展示，每个任务类型下展示多个“执行计划”。
- 计划行展示目标媒体库数量、计划时间、串行/并发提示、启停状态和运行摘要。
- 配置计划时使用带搜索的媒体库多选；新建计划即完成媒体库迁移，不需要逐库编辑。
- 运行记录继续按媒体库显示，并可从计划摘要进入对应运行记录；自定义计划提供删除入口，删除前明确提示媒体库将回到默认计划。
- 实时增量扫描不出现在 Cron 计划列表中。

## Commands

```bash
cargo test --locked --test scheduled_tasks
cargo test --locked --test libraries_api
cargo test --locked --test storage
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
pnpm --dir web install --frozen-lockfile
pnpm --dir web test
pnpm --dir web build
uname -m
```

## Project Structure

- `migrations/` and `migrations-postgres/`: plan tables and legacy data migration。
- `src/storage/`: plan persistence, membership validation and legacy mirror synchronization。
- `src/application/scheduled_tasks.rs`: plan scheduling and per-library dispatch。
- `src/api/admin_handlers.rs` and `src/api/admin.rs`: administrator plan endpoints。
- `web/src/features/admin/AdminOperationsPage.tsx`: task type/plan management UI。
- `tests/` and `web/tests/`: migration, API, dispatch and UI regression tests。

## Code Style

计划配置和运行任务分离，应用层使用明确的计划 ID；SQL 只留在 `storage`：

```rust
let plan = database
    .find_scheduled_task_plan(plan_id)
    .await?
    .ok_or(ScheduledTaskPlanError::NotFound)?;
for library in plan.libraries {
    dispatch_library_task(&library.id, &plan.task_type).await?;
}
```

计划级接口使用 camelCase JSON 字段、稳定的全大写任务类型和一致的结构化错误响应。

## Testing Strategy

- 单元测试：计划分组键、默认计划选择、重复媒体库归属和调度键。
- SQLite 集成测试：空库迁移、旧配置分组、计划创建/更新/媒体库移动和镜像同步。
- API 测试：管理员权限、分页、Cron 校验、冲突错误、立即执行、删除计划和旧 API 兼容。
- Web 测试：按任务类型展示多个计划、创建/编辑/删除计划、多选媒体库和运行按钮。
- 完成阶段运行完整 Rust/Web 质量门；PostgreSQL 空库若本机不可用则明确记录未实测。

## Boundaries

- Always：保留旧配置镜像；计划和媒体库关联使用事务；运行任务继续按媒体库拆分；新行为必须有自动化测试。
- Ask first：删除或改变旧 `/admin/scheduled-tasks` 字段；改变全量扫描资源锁；改变插件配置所有权；增加核心依赖。
- Never：让计划聚合绕过全局扫描锁；让一个媒体库同时属于同一任务类型的多个计划；在用户请求路径运行整库扫描；泄露凭据或完整外部 URL。

## Success Criteria

- 10 个媒体库使用同一任务类型时，管理页面按一个或多个执行计划展示，而不是展示 10 条独立配置卡片。
- 每个执行计划可以有独立 Cron 和媒体库范围，且不同计划不会相互覆盖。
- 自定义计划可以删除，计划内媒体库和其 Cron/启停配置镜像会回到对应默认计划；默认计划和全局插件计划不会被删除。
- 一个计划包含多个媒体库时，立即执行和定时执行都按媒体库创建独立运行任务，并受现有扫描队列限制。
- 现有数据库从空库和已有库均可完成迁移；原有每库计划行为保持不变。
- 旧 API 测试、Rust/Web 测试、格式、Clippy 和 Web 构建通过。

## Verification Record

2026-09-07，分支 `codex/scheduled-task-plans` 在本机 `uname -m=arm64` 上完成验证：

- `cargo test --locked --all-targets -- --test-threads=1`：无失败；库测试 427 passed、4 ignored，其他已执行目标均通过；PostgreSQL 集成测试 4 项因本机无 PostgreSQL 实例而忽略，性能门测试 3 项按项目规则忽略。
- `cargo build --locked`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`：通过。
- `pnpm --dir web install --frozen-lockfile`、`pnpm --dir web test`、`pnpm --dir web build`：通过；Web 静态测试 104 passed，Vitest 463 passed。
- 本次验证仅代表本机 ARM64 环境，不外推 NAS/x86 性能。

## Implementation Slices

1. Schema and migration：新表、镜像列、旧配置分组迁移和迁移测试。
2. Storage/application：计划查询、事务更新、默认计划加入和计划派发。
3. Admin API：计划列表、创建、更新、立即执行和兼容旧接口。
4. Web：任务类型/执行计划层级、计划编辑器和多选媒体库。
5. Full verification：回归、文档、质量门和 ARM 架构记录。
