# ADR-021：后台任务的资源隔离与进度通知

## Status

Accepted

## Date

2026-08-21

## Context

Lux 的 watcher 注册和整库元数据任务都包含跨越较大外部资源边界的工作：文件系统 watcher
注册可能受平台实现影响，元数据任务则会并行调用刮削器、读取本地数据并写入 SQLite。
如果把 watcher 注册直接放到 Tokio 核心 worker，或让每个任务独立扩张并发，启动和前台请求
会受到不可预测的延迟影响。逐条发布 metadata 进度事件还会造成管理员页面重复失效相同查询。

## Decision

- watcher 初始化使用有界的专用 OS 线程；初始化线程数量由 semaphore 限制。Tokio 任务只负责
  等待结果和生命周期管理，不在核心 worker 上调用同步 watcher 构造器。
- metadata 任务使用进程级 semaphore 限制总 worker 数量，并使用 RAII owner guard 防止同一任务
  在进程内被重复启动。任务状态仍以数据库为准，进程重启时将遗留 `RUNNING` 条目重排为
  `PENDING`。
- metadata 任务持久化 `job_scope` 和 `library_id`，整库任务创建时明确写入 `LIBRARY`，历史
  任务不通过条目数量猜测范围。
- 进度事件按 job ID 节流，完成、失败和取消事件立即发布。`metadata` 作用域只刷新元数据内容
  相关查询，任务列表由 `jobs` 作用域刷新，避免同一查询重复失效。
- 外部图片请求只对明确的瞬时失败进行有限次退避重试；永久 HTTP 错误不重试。

## Alternatives Considered

### 在 async 任务中直接构造 watcher

实现简单，但同步系统调用会阻塞 Tokio 核心 worker，故拒绝。

### 继续使用 Tokio blocking pool

不会阻塞核心 worker，但 blocking pool 可能被扫描、探测或其他长任务占满，导致 watcher
初始化排队并拖延启动。专用线程加有界 semaphore 将该资源边界独立出来。

### 每个 metadata 任务单独使用最大并发

单任务看似更快，但多个整库任务会争抢连接池、CPU 和外部刮削器配额。采用全局上限和动态
任务并发的组合。

## Consequences

- watcher 初始化会产生少量专用线程，但线程数量有界且不占用 Tokio blocking pool。
- metadata 任务吞吐可能低于单任务最大并发，但前台延迟和外部服务压力更可预测。
- 单进程 owner guard 不能替代数据库状态转换；未来多进程部署仍需数据库级 lease/token。
- 进度事件减少后，前端依赖最终事件保证完成、失败和取消状态及时刷新。
