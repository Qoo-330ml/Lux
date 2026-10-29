# 后台任务盘点与空闲唤醒评估（分析项 C2）

本文盘点 `src/**` 非测试代码中**长期运行的后台循环**，量化空闲状态下的周期性唤醒与数据库访问，
并在本机 arm64 上做一次实测，最后按 A/B/C 分级给出建议。

**本任务只做盘点与量化，不改任何 `src/**`。** 是否把分散的轮询收敛到统一调度器，由后续任务决定。

---

## 1. 统计口径与方法

### 1.1 扫描范围

- `src/**` 下全部 `.rs` 文件，排除 `*_tests.rs`、`tests.rs`，以及各文件末尾的 `#[cfg(test)] mod tests { ... }`
  块（用花括号配对切出块边界后再统计，避免把测试代码里的 `spawn`/`sleep` 算进来）。
- "长期运行的后台循环"定义：由进程启动期或状态构造期创建、生命周期与进程相同（或与某个常驻服务相同）、
  在循环里等待时间或事件的异步任务。**按请求/按作业创建的循环（重试、分页、`select!` 收尾、子进程
  reaper）不算**，单独在 §3 归类。

### 1.2 重新统计的基线数字

任务描述里的静态扫描数字与本仓库重新统计的差异如下（同一台机器、同一 commit）：

| 指标 | 任务描述 | 重新统计（`src/**` 全部行） | 非测试代码 |
| --- | --- | --- | --- |
| `tokio::spawn` | 约 103 | 130 行命中 | **76 处**（54 行在 `#[cfg(test)] mod tests` 里） |
| `tokio::time::interval` | 约 88（含 `sleep` 循环） | 12 行命中 | 12 处（全部在非测试代码） |
| `sleep`（`tokio::time::sleep` / `time::sleep`） | — | 96 行命中 | **58 处** |
| `loop {` | — | 154 行命中 | 141 处（绝大多数是重试/分页/解析循环，不是后台循环） |

"约 103 处 `tokio::spawn`"更接近"按文件过滤 `_tests.rs` 后 110 行命中"这个口径；按 `#[cfg(test)]`
块精确切分后是 76 处。**76 处 `spawn` 里只有一小部分属于常驻后台循环**（其余是每请求、每作业、
一次性重建、阻塞桥接）：本文 §2 只认定 27 个常驻循环，其中 15 个空闲时会周期性唤醒。

### 1.3 逐项取证方式

对每个候选点，沿 `spawn` 调用点向上找到创建者（`main.rs` 的启动序列、`AppState` 构造、或在
`src/api/legacy.rs` 的 `start_*` 方法里），再确认：

1. 循环里等待的是**固定时长**还是**事件**（`Notify` / `watch` / `broadcast` / `mpsc` / socket）；
2. 空闲（没有待处理工作、没有客户端连接）时是否仍然按周期唤醒；
3. 每次唤醒是否访问数据库或磁盘；
4. 关停路径：是 `background_loop::BackgroundLoop`（`watch` 信号 + 受监督重启）、手写
   `watch::Sender<bool>` + `Arc<Mutex<Option<JoinHandle>>>`、还是裸 `tokio::spawn`（handle 直接丢弃，
   无法关停）。

关停路径的汇总见 §2.3 与 §2.4。

---

## 2. 长期运行的后台循环（进程常驻）

### 2.1 周期性唤醒的常驻循环（**空闲时也会醒**）

"空闲唤醒"指进程运行但没有扫描/刮削/播放/请求、也没有客户端连着 SSE 时的唤醒。"周期"列给出
常量名与值，以及定义所在行。"空闲时 DB/磁盘"列只统计**空闲路径**上真正发出的查询/文件操作。

| # | 文件:行 | 用途 | 周期（常量 = 值） | 空闲唤醒 | 空闲时 DB/磁盘 | 关停方式 | 可事件驱动化 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | `src/application/scanner/jobs/local_metadata.rs:889`（循环）/`:1053`（等待） | 扫描本地元数据 outbox 消费 + NFO 回填页 | `SCAN_LOCAL_METADATA_OUTBOX_IDLE_FALLBACK` = **250 ms**（`:17`）+ 同一 `Notify` 双路等待 | **是，4 次/秒** | **是**：每轮 2 次 claim 查询（outbox、backfill） | `background_loop::supervise_with_restart`（`:1570`），**没有 shutdown 信号**，handle 被丢弃，只能靠进程退出 | 可：空闲只 `await notify`，跨进程写入用低频兜底（≥10 s）或 DB 通知 |
| 2 | `src/application/collections.rs:264`（循环）/`:405`（等待） | 全局合集同步 + 电影 NFO 合集引用对账 | `GLOBAL_COLLECTION_SYNC_RETRY_DELAY` = **1 s**（`:233`），纯轮询、无 `Notify` | **是，60 次/分** | **是**：每轮 ≥2 查询（对账候选、待同步作业） | `GlobalCollectionSyncWorker`（`:239`，`watch` + `JoinHandle`），纳入 `shutdown_background_workers_checked` | 可：`enqueue` 时 `notify_one` |
| 3 | `src/application/catalog_read_model.rs:162`（循环）/`:158`（ticker） | `catalog_items` outbox 增量消费（读模型） | `CATALOG_ITEMS_INCREMENTAL_INTERVAL` = **1 s**（`:147`） | **是，60 次/分** | **是**：`catalog_consumer` 查询 + `drain` + 周期性 `trim`/统计重建 | 裸 `tokio::spawn`（`:157`），handle 丢弃，**无关停** | 可：outbox `seq` + `Notify`，写侧在同一进程内 |
| 4 | `src/application/catalog_read_model.rs:228`（循环）/`:226`（ticker） | 搜索重建索引 outbox 消费（FTS） | `SEARCH_REINDEX_INTERVAL` = **1 s**（`:141`） | **是，60 次/分** | **是**：`drain_search_reindex` + `trim` | 裸 `tokio::spawn`（`:225`），**无关停** | 可：同上 |
| 5 | `src/application/playback/session.rs:242`（循环）/`:241`（interval） | 回收失活/过期 Web 播放会话并停 HLS | `WEB_PLAYBACK_CLEANUP_INTERVAL` = **5 s**（`:47`） | **是，12 次/分** | **是**：`take_inactive_web_playback_sessions` + `take_expired_web_playback_sessions` | 裸 `tokio::spawn`（`:237`），**无关停** | 可：只在有活动会话时跑，或改由会话心跳驱动 |
| 6 | `src/application/webhooks.rs:160`（循环）/`:170`（等待） | webhook 投递轮询 + 历史保留期清理 | `DELIVERY_POLL_INTERVAL` = **5 s**（`:39`）；`HISTORY_RETENTION_INTERVAL` = **1 h**（`:40`） | **是，12 次/分** | **是**：`process_ready_deliveries` 查询；每小时一次 `DELETE` | `BackgroundLoop`（`:629`），纳入 shutdown | 可：投递入队时 `notify` |
| 7 | `src/application/mail_worker.rs:106`（循环）/`:135`（等待） | 邮件 outbox 轮询（SMTP） | `MAIL_QUEUE_POLL_INTERVAL` = **5 s**（`:26`） | **是，12 次/分** | **是**：`claim_mail_outbox` | 自有 `watch` + `JoinHandle`，`main.rs:265` 显式 `mail_worker.shutdown().await` | 可：入队 `notify` |
| 8 | `src/application/content_restriction_index.rs:132`（循环）/`:198`（等待） | 内容限制索引增量构建 | `CONTENT_RESTRICTION_INDEX_IDLE_RETRY` = **10 s**（`:93`）；错误重试 1 s（`:94`） | **是，6 次/分** | **是**：`index_batch` 查询 | 自有 `watch` + `JoinHandle`（`start` `:104`、`shutdown` `:116`），纳入 shutdown | 可：条目变更时 `notify` |
| 9 | `src/application/scan_fingerprint_migration.rs:50`（循环）/`:415`（等待） | 扫描指纹迁移（一次性迁移，但实现为常驻循环） | `IDLE_INTERVAL` = **10 s**（`:20`）；启动延迟 `START_DELAY` = 60 s（`:18`）；库列表刷新 `LIBRARY_REFRESH_INTERVAL` = 60 s（`:21`） | **是，6 次/分**（迁移完成后仍继续） | **是**：每库一次 `ensure_scan_fingerprint_migration_state`，每 60 s 一次库列表查询 | 由 `ShutdownReason` `watch` 驱动（`main.rs:248`） | 可：所有库 `completed_at` 齐了就直接退出循环（一次性工作不该常驻） |
| 10 | `src/application/scheduled_tasks.rs:167`（循环） | 计划任务（cron/interval）调度 | `POLL_INTERVAL` = **15 s**（`:49`） | **是，4 次/分** | **是**：≥3 查询/轮（推荐统计、计划分页、缩略图重试） | `BackgroundLoop`（`:249`），纳入 shutdown | 需设计：按下一计划时间睡眠（可跨分钟对齐），或由配置变更 `notify` + 单次定时器 |
| 11 | `src/application/watch.rs:247`（循环）/`:244`（interval） | 媒体库根目录监听刷新 | `LIBRARY_ROOT_REFRESH_INTERVAL` = **30 s**（`:36`） | **是，2 次/分** | **是**：`list_enabled_library_roots` | `spawn()`（`:226`）返回的 `JoinHandle` 在 `api/legacy.rs:711` 被丢弃；`notify` crate 的监听器另有自有线程 | 已部分事件驱动（`library_change_receiver.changed()`），可把 30 s 兜底加长到分钟级 |
| 12 | `src/application/user_cleanup.rs:36`（循环）/`:33`（interval_at） | 过期用户、用户活动清理 | `USER_CLEANUP_INTERVAL` = **60 s**（`:9`） | **是，1 次/分** | **是**：3 查询/轮（保留期配置、活动清理、过期用户） | 裸 `tokio::spawn`（`:31`），**无关停** | 可：改成小时级（保留期是天级，分钟级探测没有意义） |
| 13 | `src/application/identity_merge.rs:81`（循环） | 重复媒体身份自动合并 | `IDENTITY_MERGE_INTERVAL` = **120 s**（`:24`），失败重试 300 s（`:25`）；已有 `Notify` | **是，0.5 次/分** | **是**：每轮一次合并候选查询 | 自有 `watch` + `JoinHandle`（`:45`），纳入 shutdown | 已基本事件驱动（`:123` `notify.notified()`），可去掉固定兜底 |
| 14 | `src/application/plugins.rs:252`（循环）/`:251`（interval） | 登录背景图定时刷新（选中插件） | `LOGIN_BACKGROUND_REFRESH_INTERVAL` = **300 s**（`:93`） | **是，0.2 次/分** | 是（有选中插件时读插件结果） | 裸 `tokio::spawn`（`:250`），**无关停** | 可：已有 `login_background_refresh_notify`，定时兜底可再加长 |
| 15 | `src/storage/watch_state_archive.rs:500`（循环）/`:498`（interval） | 游离观看状态/播放历史重挂 | `WATCH_STATE_RECONCILE_INTERVAL` = **300 s**（`:35`），初始延迟 60 s（`:36`） | **是，0.2 次/分** | **是**：2 查询/轮 | 裸 `tokio::spawn`（`:496`），**无关停** | 可：改由写入侧 `notify` |
| 16 | `src/storage/database_maintenance.rs:650`（循环）/`:648`（interval） | 查询统计刷新（ANALYZE / PG 统计） | `STATISTICS_REFRESH_INTERVAL` = **900 s**（`:477`），初始 10 s（`:478`） | **是，0.07 次/分** | 是：ANALYZE / `pg_stat` 查询 | 裸 `tokio::spawn`（`:646`），`LUX_DATABASE_MAINTENANCE=0` 可整体关闭 | 保持 |
| 17 | `src/storage/database_maintenance.rs:813`（循环）/`:811`（interval） | 空间回收维护轮次（VACUUM 等） | `DEFAULT_MAINTENANCE_INTERVAL_SECONDS` = **86400**（`:11`），初始 600 s（`:13`），可用环境变量覆盖 | 否（天级） | 是（低频重 IO） | 裸 `tokio::spawn`（`:808`） | 保持 |
| 18 | `src/storage/database_cleanup.rs:1949`（循环）/`:1947`（interval） | 任务历史保留期清理 | `TASK_HISTORY_RETENTION_INTERVAL` = **24 h**（`:915`），初始 15 min（`:913`） | 否（天级） | 是（低频批量 `DELETE`） | 裸 `tokio::spawn`（`:1945`） | 保持 |

> 第 9 项值得单独强调：扫描指纹迁移是一次性数据迁移，但实现成"没有候选就 10 s 后再看一次"的常驻轮询，
> 迁移完成后仍然每 10 s 醒来一次并按库查询状态。这是"一次性任务被写成常驻轮询"的典型形态。

### 2.2 空闲时不唤睡的常驻循环（事件驱动 / 连接驱动 / 工作驱动）

| # | 文件:行 | 用途 | 唤醒方式 | 空闲唤醒 | 空闲时 DB/磁盘 | 关停方式 | 事件驱动化 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 19 | `src/discovery.rs:137`（循环） | LAN 发现 UDP 应答（Emby 兼容发现） | `socket.recv_from` | 否 | 否 | `watch` 信号（`discovery.run(shutdown_rx)`，`main.rs:239` 持 `JoinHandle` 并 `await`） | 已是事件驱动 |
| 20 | `src/api/admin/admin_logs_events.rs:380` / `:447` | 管理端 / 用户 SSE 心跳（`keep-alive`） | `interval` 15 s，仅连接存在时 | 否（无连接就无任务） | 否 | 连接结束即退出 | 保持（心跳是 SSE 必需） |
| 21 | `src/application/provider_cache.rs:331`（`persist_loop`，循环 `:333`） | 供应商缓存落盘 | `Notify` + 2 s 去抖（`CACHE_PERSIST_DEBOUNCE`，`:24`） | 否 | 是（只在脏时写文件） | 裸 `tokio::spawn`，进程退出即止 | 已是事件驱动 |
| 22 | `src/application/actor_enrichment.rs:73`（循环） | 演职员去抖补全队列 | `mpsc` + `CancellationToken` | 否 | 否 | `CancellationToken`（随队列 drop 取消） | 已是事件驱动 |
| 23 | `src/application/probe/concurrency.rs:314`（循环） | 探测准入排空 | `wait_for_change_since`（`Notify`） | 否 | 否 | 会话结束 | 已是事件驱动 |
| 24 | `src/application/database_diagnostics.rs:60` | 启动后一次性数据库诊断 | `sleep(5 min)` 后跑一次即结束 | 否 | 是（一次） | 无 | 一次性，保持 |
| 25 | `src/application/catalog_read_model.rs:52` / `:74` / `:104` | 启动期 `catalog_items` / 用户"下一集"全量重建 | 一次性 | 否 | 是（一次，重） | 裸 `tokio::spawn` | 一次性，保持 |
| 26 | `src/storage/database_maintenance.rs:783` | PG `pg_stat_statements` 扩展创建 | 一次性 | 否 | 是（一次） | 无 | 一次性，保持 |
| 27 | `src/storage/database_cleanup.rs:1377`（`spawn` 在 `:1386`） | 软删除媒体条目过期清扫 | 启动/扫描结束后触发的一次性遍历（批间 200 ms 节流） | 否 | 是（成批） | 无 | 需求驱动，保持 |

### 2.3 关停路径汇总（现状）

`shutdown_background_workers_checked`（`src/api/legacy.rs:242`）只排空 7 类 worker：内容限制索引
（`content_restriction_index_worker`）、身份合并（`identity_merge_worker`）、探测运行时、元数据重识别、
全局合集同步（`global_collection_sync_worker`）、webhook、计划任务。`main.rs` 另外显式排空
LAN 发现与邮件 worker。

**没有被排空（handle 丢弃、只能靠进程退出）的常驻循环**有 8 个：

| 循环 | 位置 | 影响 |
| --- | --- | --- |
| 本地元数据 outbox | `scanner/jobs/local_metadata.rs:1570` | 优雅停机的 `cancel_incomplete_jobs_for_shutdown` 之后仍可能再 claim 一个新批次 |
| `catalog_items` 增量消费 | `catalog_read_model.rs:157` | 停机瞬间可能正在写读模型 |
| 搜索重建索引 | `catalog_read_model.rs:225` | 同上 |
| Web 播放会话回收 | `playback/session.rs:237` | 停机瞬间可能正在停 HLS |
| 用户清理 | `user_cleanup.rs:31` | 停机瞬间可能正在删数据 |
| 登录背景图刷新 | `plugins.rs:250` | 停机瞬间可能在读插件 |
| 游离状态重挂 | `watch_state_archive.rs:496` | 停机瞬间可能在写观看状态 |
| 库根目录监听刷新 | `watch.rs:226`（`spawn()` 的 `JoinHandle` 被丢弃） | `notify` crate 的每个根目录监听线程在停机后随进程退出 |

这不是本任务的修复范围（属于后续"统一调度/关停"设计），但记录在此：**空闲唤醒最密集的 4 个循环
（250 ms / 1 s / 1 s / 5 s）全部属于"无 `watch` 信号、无 shutdown"的那一类**。

### 2.4 常驻循环数量小结

- 常驻后台循环：**27 个**（§2.1 的 18 个周期性 + §2.2 的 9 个事件/工作驱动）。
- 其中空闲时会周期性唤醒的：**15 个**（§2.1 第 1–15 项；第 17–18 项是天级，第 16 项是刻钟级，
  都不构成空闲功耗问题）。
- 这 15 个里关停路径缺失的：**8 个**（§2.3 表格）。

---

## 3. 按作业/按请求运行的长循环（不计入空闲唤醒）

这些循环有 `loop` 或 `sleep`，但只在有工作、有连接、有子进程时存在，空闲时不产生唤醒。列在此处是
为了说明"为什么静态扫描数出来的 100+ 处 `sleep` 里只有一小部分影响空闲功耗"。

| 类别 | 代表位置 | 周期/等待 | 说明 |
| --- | --- | --- | --- |
| 扫描作业主循环 | `application/scanner/jobs/*.rs`（`lifecycle.rs:191`、`manifest_discovery.rs:265`、`postprocessing.rs:378`、`reconciliation.rs:1115` 等） | 工作驱动 | 作业停止/取消即退出 |
| 扫描/探测子进程 reaper | `application/probe/runner.rs:393`（`terminate_child_with_wait`，1 s 轮询） | 1 s，仅子进程存活期间 | 子进程退出即结束 |
| 探测准入 | `application/probe.rs:1070`（`PROBE_ADMISSION_TICK` = 1 s，`:31`） | 1 s，仅探测会话期间 | 会话结束即退出 |
| 播放 HLS 会话 | `application/playback/hls.rs`（`:984`、`:996`、`:1147` 等） | 100 ms~1 s，仅播放期间 | 随会话结束 |
| 元数据刮削/重识别 | `application/reidentify.rs:288`（去抖窗口 1 s，`:46`）、`:690`、`:1436` 等 | 事件 + 去抖 | 队列空即阻塞在 `recv()` |
| 元数据作业页刷新 | `application/scanner/jobs/local_metadata.rs:1268` | `LOCAL_METADATA_JOB_REFRESH_FALLBACK` = 5 s（`:12`），仅作业运行期间 | 作业结束即退出 |
| 配置化 STRM 探测启动等待 | `application/strm_probe.rs:479` | 500 ms，仅"已有别的探测在跑"时 | 启动成功即 `return` |
| Emby 迁移 | `application/emby_migration_service.rs:420`（限流重试） | 250 ms~4 s 退避 | 作业结束即退出 |
| 缩略图重试轮询 | `application/scheduled_tasks.rs:879`（`THUMBNAIL_METADATA_JOB_STATUS_FALLBACK` = 1 s，`:54`） | 1 s，仅等某个元数据作业结束 | 由调度器触发，任务结束即退出 |
| 库删除批次 | `application/library_deletion.rs:344` | `yield_now` 循环 | 作业结束即退出 |
| 图片/缩略图并发与重试 | `application/images.rs:103`（信号量）、`:1256`（重试退避）、`application/thumbnails.rs:842` | 工作驱动 | 请求/作业结束即退出 |
| 文件监听去抖 | `application/watch.rs:127`（`DEFAULT_DEBOUNCE` = 250 ms，`:34`） | 事件驱动 | 由 `notify` 事件触发 |
| 事件合并窗口 | `application/admin_events.rs:147` / `:217`（1 s 去抖）、`application/home.rs:398`（100 ms 去抖） | 每次发布事件时创建 | 窗口结束即退出 |
| 重试/退避/解析循环 | `storage/users.rs:1068`、`application/scraper.rs:1357`、`application/nfo.rs`（多处）、`application/people/helpers.rs:1290` 等 | 工作驱动 | 不属于后台任务 |

---

## 4. 结论（盘点部分）

1. 空闲唤醒高度集中：**250 ms 的本地元数据 outbox 兜底 + 三个 1 s 的 outbox/同步消费者**贡献了绝大部分
   唤醒（详见下一节的估算）。
2. 这些高频循环几乎都是**纯轮询的 outbox 消费者**，而 in-process 的 `Notify` 已经存在（本地元数据、
   身份合并、登录背景图），说明改成事件驱动在结构上可行，缺的是跨进程/跨路径写入的兜底策略。
3. 关停路径与唤醒频率呈负相关：最密的 4 个循环恰好都没有 `watch` 信号。
