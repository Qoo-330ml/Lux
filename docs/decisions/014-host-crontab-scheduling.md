# ADR-014：由宿主机 crontab 管理计划任务

## Status

Accepted

## Date

2026-08-10

## Context

Lux 部署在 Docker 中，计划任务的时间配置属于部署环境，而扫描、刮削、媒体探测和旁车写回属于
Lux 的持久化后台任务系统。把两者都放进 Lux 会增加内部调度器、cron 解析、时区和重启语义；把
任务直接交给宿主机 crontab 又不能提供任务进度、取消、重试和去重。

## Decision

宿主机 crontab 是计划时间的唯一来源。Lux 不运行内部计划轮询器、不解析 cron 表达式，也不让
crontab 直接操作数据库或媒体目录。

宿主机 crontab 通过带有独立高熵令牌的
`POST /api/v1/cron/tasks/{ownerType}/{ownerId}/{taskType}` 调用 Lux。Lux 验证任务注册项、启用
状态和任务类型，然后把工作加入已有的持久化后台任务服务。计划任务页继续展示注册项、启停状态、
运行记录和错误；不修改宿主机 crontab。

令牌由 `LUX_CRON_TOKEN` 或 `/config/cron-token` 提供，不写入日志、数据库、任务事件或 API 响应。
历史 `cron_or_interval`、`reconciliation_schedule` 和 `metadata_schedule` 字段保留用于兼容，
不再作为 Lux 的调度输入。

## Alternatives Considered

### Lux 内部 Tokio 调度器

不采用。它需要额外定义 cron 语法、时区、夏令时、错过执行和计划游标的持久化，并且计划配置
与 Docker 宿主环境容易分离。

### crontab 直接执行扫描或访问 SQLite

不采用。它绕过 Lux 的认证、任务队列、路径安全、并发配额、进度和恢复逻辑。

## Consequences

- 部署者需要在宿主机维护 crontab，并在配置目录安全保存 cron 令牌。
- Lux 的任务入队接口保持无状态；服务重启后，已经入队的任务仍按原有持久化模型恢复。
- Lux 管理页不能代表宿主机是否真的安装了某条 crontab，只能显示任务注册和启停状态。
- 标准 cron 表达式、时区和错过执行策略由宿主机 cron 实现负责。
