# LUX-044：每库扫描计划与资源配额实施计划

## 范围

为每个媒体库提供默认开启的实时监听、全量调和/元数据任务注册和扫描/探测并发配额。实时监听不再是可关闭的运行时开关；历史 `realtimeWatchEnabled` 与 `incrementalSchedule` 字段仍保留在库详情/API 中以兼容旧客户端，但后者始终为空且不参与调度。局部增量扫描由实时文件事件触发，不作为计划配置持久化。实时监听之外，媒体库可以单独开启实时新资源 `FILL_MISSING` 元数据自动补全；全量调和和元数据的执行时间由宿主机 crontab 管理。

## 规则

- 计划属于具体 library，两个库互不覆盖。
- `reconciliationSchedule`、`metadataSchedule` 作为旧客户端兼容字段返回 `null`；实时增量扫描不提供计划字段。
- 任务启停配置不要求重启；宿主机 crontab 通过 cron 入队接口读取最新注册项状态。
- `realtimeMetadataAutoMatchEnabled` 默认关闭，不改变 watcher 的持续运行；开启后只处理实时增量任务影响的媒体条目。
- `scanConcurrency`、`probeConcurrency` 必须为正数，服务端限制合理上限。
- 资源配额只限制配置；插件计划任务复用已登记的全局任务，由宿主机 crontab 调用 cron 入队接口。

## 增量任务

- [x] 新增 `scheduled_task_configs` migration 和 storage upsert。
- [x] 实现管理员 PATCH 媒体库设置 API。
- [x] 验证每库独立计划、元数据计划独立和运行时更新。

## 验证门

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- ARM64 本机验证两个媒体库设置互不影响，更新后无需重启可查询。

实现记录：schema 12 新增独立任务配置表；`PATCH /api/v1/admin/libraries/{libraryId}` 使用事务更新库兼容字段和任务配置。Lux 不解析 cron，资源配额仅做 1-64 的配置校验；cron 入队接口在同一个运行中的服务进程内验证注册项、启停状态和重复任务处理。

## 明确不做

- 不为媒体库系统任务实现 cron 解析器；定时表达式属于宿主机 crontab，Lux 只提供受保护的入队接口。
- 不实现跨库全局 worker pool；具体任务执行器后续继续复用这些配额。
