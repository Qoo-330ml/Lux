# LUX-044：每库扫描计划与资源配额实施计划

## 范围

为每个媒体库提供默认开启的实时监听、增量/全量调和/元数据计划和扫描/探测并发配额。实时监听不再是可关闭的运行时开关；历史字段仍保留在库详情/API 中以兼容旧客户端。计划配置独立持久化。

## 规则

- 计划属于具体 library，两个库互不覆盖。
- `incrementalSchedule`、`reconciliationSchedule`、`metadataSchedule` 可独立设置或清空。
- 计划配置不要求重启；下一次 watcher/job 读取数据库最新值。
- `scanConcurrency`、`probeConcurrency` 必须为正数，服务端限制合理上限。
- 资源配额只限制配置，不在本阶段实现完整调度器。

## 增量任务

- [x] 新增 `scheduled_task_configs` migration 和 storage upsert。
- [x] 实现管理员 PATCH 媒体库设置 API。
- [x] 验证每库独立计划、元数据计划独立和运行时更新。

## 验证门

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- ARM64 本机验证两个媒体库设置互不影响，更新后无需重启可查询。

实现记录：schema 12 新增独立任务配置表；`PATCH /api/v1/admin/libraries/{libraryId}` 使用事务更新库兼容字段和三类任务配置。计划不解析 cron，资源配额仅做 1-64 的配置校验；集成测试在同一个运行中的服务进程内验证两个库互不覆盖，并验证 `null` 清空计划。

## 明确不做

- 不实现 cron 解析器或后台定时调度循环。
- 不实现跨库全局 worker pool；具体任务执行器后续继续复用这些配额。
