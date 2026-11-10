# ADR-053：计划任务内的评分同步（`RATINGS_SYNC`）

- 状态：已接受
- 日期：2026-11-06

## 背景

`org.lux.trakt-ratings` 插件在清单里声明了一个媒体库级计划任务 `RATINGS_SYNC`，宿主侧的任务登记已经是通用的
（`register_plugin_library_scheduled_tasks`），但执行路径的 `normalize_scheduled_task_request`、
`run_task_with_config` 和 `scheduler_schedule_values` 只认少数几个类型，触发时返回 `UnsupportedTask`。

评分同步的特点是：

- 需要向外部服务（Trakt，经插件）按条目查询，单个条目要 350 ms 以上的上游间隔，整库要跑很久；
- 唯一的持久化副作用是 NFO 旁车的评分字段；重跑会从零重算，没有增量语义；
- 写入使用任务 A 的 `NfoWriteService::write_item_ratings`，该写入会打 `internal_write` 标记，因此文件监听
  不会重扫，宿主必须自己触发一次本地元数据刷新。

已存在的弹幕匹配和片头片尾检测各自有独立作业表、API、取消/重试语义和前端页面。为评分同步复制这一整套
（第四张作业表、第四组端点、第四个前端页面）会显著放大表面，而 v1 真正需要的只是“按周跑一次、能看日志”。

## 决定

1. `RATINGS_SYNC` 作为媒体库级任务类型放行：`schedule.rs` 新增常量与默认计划 `0 4 * * 0`（UTC 每周日
   04:00），并加入三处白名单（请求归一化、`run_task_with_config`、调度器计划过滤）。
2. 执行体是 `RatingsSyncService`，进程内后台任务：`start(library_id, plugin_id)` 只做前置校验（媒体库存在且
   启用、插件已安装且启用、`clientId` 非空）、进程内重入保护（同一媒体库一个运行）和 `tokio::spawn`，
   立即返回一个 UUID run id；HTTP 层返回 `202` 和 `{ "runId", "libraryId" }`。
3. **不建表、不加迁移**。v1 没有作业行、没有进度端点、没有重启恢复、没有“条目上次同步时间”标记；运行只能
   通过 `tracing` 汇总日志观测。重启后遗留的工作只是文件系统上已写好的 NFO，保持有效。
4. 分页遍历使用 keyset（按条目 id），每页 500；每页之间检查媒体库是否仍存在且启用。先做只读的
   `NfoWriteService::item_has_nfo` 过滤，没有旁车的条目不请求上游。
5. 查询构造只使用可靠标识：IMDb，或 TMDb 加年份（绝不只靠标题）。剧集先在整个库上跑一遍，缓存
   `series_id → traktId`，分集通过剧集 IMDb 或该 Trakt id 查询。`matchedBy == "title+year"` 或
   `lowConfidence == true` 的结果不写盘。
6. 插件响应按不可信输入校验（名称字符集与长度、0-10 且有限的分数、非负票数、`max == 10`、条目数上限），
   任何一项不满足就跳过该条目。容器顺序原样保留，因为 Jellyfin 取最后一个非 `tomato` 条目。
7. 每批 50 条、120 秒超时；批级失败退避 5 秒、30 秒重试两次后中止本次运行；`TRAKT_RATE_LIMITED` /
   `TRAKT_UPSTREAM_ERROR` 的条目在整批结束后等 60 秒重试一轮；`TRAKT_UNAUTHORIZED` /
   `TRAKT_NOT_CONFIGURED` 中止整个运行。单条写盘错误只计失败。
8. 写完后按库根分组、每批最多 2000 个目录调用 `start_local_metadata_refresh`，`NoChanges` 视为正常；
   刷新失败只记日志，不影响已写入的 NFO。运行中期每累计约 500 个变更目录先刷一次。
9. 关停沿用宿主的后台任务约定：`ScheduledTaskService::shutdown` 先停调度循环，再向所有运行发停止信号
   并有界等待（10 秒），然后才轮到插件进程和数据库关闭。

## 备选方案

### 复制弹幕/章节检测的持久化作业模型

需要新表、迁移（SQLite 与 PostgreSQL 各一份）、作业 API、取消/重试和界面。v1 没有“上次同步时间”标记，
一次运行本来就是全量重查，作业行除了进度展示没有别的用途，因此不做；等需要断点续跑或界面进度时再单独设计。

### 在 HTTP 请求里同步完成整库同步

会违反“用户请求里不扫库、不做上游调用”的既有约束，也会被上游速率拖到超时，拒绝。

### 用文件监听代替主动刷新

任务 A 的写入带 `internal_write` 标记，监听器会主动忽略自写文件；依赖监听会让评分永远不进入目录。

### 每轮都调用插件并写入 `movie.nfo`

写盘器明确不创建新旁车：合成的评分旁车会被下一次扫描当作本地元数据而影响识别。没有旁车的条目直接跳过。

## 安全与后果

- 日志只包含 run id、媒体库 id、插件 id、条目 id、计数和耗时；不记录 `clientId`、完整 URL 或查询串。
- 插件输出不被信任：校验失败的条目不会产生任何写盘。
- 已知限制：没有“上次同步时间”标记，所以每次运行都会重查整库（默认因此是每周而不是每天）；运行没有持久化状态
  和界面进度，只能靠日志；进程重启会丢掉正在进行的运行（已写入的 NFO 仍然有效）。
- 运行在库根目录下的旁车（相对目录为空）无法通过 `start_local_metadata_refresh` 刷新，因为该接口不接受空路径；
  这类条目仍然会被写入，只是等待下一次扫描。
