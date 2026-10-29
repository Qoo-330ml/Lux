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

---

## 5. 空闲唤醒估算

### 5.1 估算方法

- 只统计 §2.1 的周期性循环：**每个循环每个周期恰好产生一次唤醒**，`唤醒/分钟 = 60 / 周期秒数`。
- 天级任务也折算（`60/86400 ≈ 0.0007`），列出但视为可忽略。
- 不计入工作驱动循环（§3），不计 SSE 心跳（没有客户端就没有任务）。
- 假设循环体耗时远小于周期（空闲时成立：§6.2 的实测显示一分钟内 SQLite 字节码执行时间只有约 5–10 ms，
  说明这些查询确实很轻）。
- 场景：默认配置、SQLite、空库、无客户端连接、没有扫描/刮削/播放。

### 5.2 空闲唤醒总次数与贡献排名

| 排名 | 循环 | 周期 | 唤醒/分钟 | 占总量 |
| --- | --- | --- | --- | --- |
| 1 | 本地元数据 outbox 兜底（§2.1-1） | 250 ms | **240** | 50.4 % |
| 2 | `catalog_items` 增量消费（§2.1-3） | 1 s | **60** | 12.6 % |
| 3 | 搜索重建索引（§2.1-4） | 1 s | **60** | 12.6 % |
| 4 | 全局合集同步（§2.1-2） | 1 s | **60** | 12.6 % |
| 5 | Web 播放会话回收（§2.1-5） | 5 s | 12 | 2.5 % |
| 6 | webhook 投递轮询（§2.1-6） | 5 s | 12 | 2.5 % |
| 7 | 邮件 outbox 轮询（§2.1-7） | 5 s | 12 | 2.5 % |
| 8 | 内容限制索引（§2.1-8） | 10 s | 6 | 1.3 % |
| 9 | 扫描指纹迁移（§2.1-9） | 10 s | 6 | 1.3 % |
| 10 | 计划任务调度（§2.1-10） | 15 s | 4 | 0.8 % |
| 11 | 媒体库根目录刷新（§2.1-11） | 30 s | 2 | 0.4 % |
| 12 | 用户清理（§2.1-12） | 60 s | 1 | 0.2 % |
| 13 | 身份合并兜底（§2.1-13） | 120 s | 0.5 | 0.1 % |
| 14 | 登录背景图刷新（§2.1-14） | 300 s | 0.2 | <0.1 % |
| 15 | 游离状态重挂（§2.1-15） | 300 s | 0.2 | <0.1 % |
| 16 | 查询统计刷新（§2.1-16） | 900 s | 0.07 | <0.1 % |
| 17 | 空间回收维护（§2.1-17） | 86 400 s | 0.0007 | ≈0 |
| 18 | 任务历史保留（§2.1-18） | 86 400 s | 0.0007 | ≈0 |
| | **合计** | | **≈ 476 次/分（≈ 7.9 次/秒）** | 100 % |

**前 4 项（250 ms + 三个 1 s）合计 420 次/分，占 88%。** 只把前 4 项改成事件驱动，就能把空闲唤醒
压到约 56 次/分。

### 5.3 空闲状态下的数据库访问量估算

空闲时真正落到磁盘的只有**读**（实测一分钟内没有任何写入，见 §6.4）。按每次唤醒的查询条数折算：

| 循环 | 每轮查询数（空闲路径） | 查询/分钟 |
| --- | --- | --- |
| 本地元数据 outbox | 2（outbox claim、backfill claim） | 480 |
| `catalog_items` 增量 | 3（consumer 水位、drain、trim） | 180 |
| 搜索重建索引 | 2（drain、trim） | 120 |
| 全局合集同步 | 2（NFO 引用对账、待同步作业） | 120 |
| Web 播放会话回收 | 2（失活、过期） | 24 |
| webhook 投递 | 1 | 12 |
| 邮件 outbox | 1 | 12 |
| 计划任务 | 3（推荐统计、计划、配置） | 12 |
| 内容限制索引 | 1 | 6 |
| 扫描指纹迁移 | 1（空库时的库列表） | 6 |
| 用户清理 | 3 | 3 |
| 媒体库根目录刷新 | 1 | 2 |
| 身份合并 / 游离状态重挂 | 1 / 2 | 0.5 / 0.4 |
| **合计** | | **≈ 980 条/分（≈ 16 条/秒）** |

在机械盘 NAS 上，每秒约 16 次读查询（命中页缓存时是纯 CPU，冷页时才触碰磁盘）本身未必让磁盘转起来，
但**它使 SQLite 连接池、WAL 调度和 tokio 运行时始终保持活跃**，这正是"空闲也无法进入低功耗状态"的机制。

---

## 6. 本机实测（arm64）

### 6.1 环境

| 项 | 值 |
| --- | --- |
| 机器 | Apple M4，10 核（4 性能核），16 GB，macOS 26.5（25F71） |
| `uname -m` | `arm64` |
| 构建 | `~/.cargo/bin/cargo build --locked`（debug，耗时 3 分 19 秒），产物 `target/debug/luxd` |
| 版本 | `luxd 0.7.2` |
| 配置 | `LUX_CONFIG_DIR=<mktemp -d>/config`（内部 `database.json` = `{"backend":"SQLITE"}`），空库 |
| 端口 | `LUX_HTTP_ADDR=127.0.0.1:18097`、`LUX_DISCOVERY_BIND_ADDR=127.0.0.1:17359` |
| 日志 | `RUST_LOG=luxd=info`（日志写入临时配置目录，未使用仓库 `config/`） |

**没有连接任何真实用户数据或仓库内 `config/` 目录**；全部数据在 `/tmp` 下的临时目录，测完已删除。

实测方式（可复现）：

```bash
TMP=$(mktemp -d /tmp/lux-idle-audit.XXXXXX)
mkdir -p "$TMP/config" && printf '{"backend":"SQLITE"}' > "$TMP/config/database.json"
LUX_HTTP_ADDR=127.0.0.1:18097 LUX_CONFIG_DIR="$TMP/config" \
LUX_DISCOVERY_BIND_ADDR=127.0.0.1:17359 RUST_LOG=luxd=info \
  ./target/debug/luxd > "$TMP/luxd.log" 2>&1 &
PID=$!
# 等 "luxd listening" 出现后再等 60 s 进入稳态，然后：
top -l 4 -s 60 -pid $PID -stats pid,csw,cpu,time,threads,idlew,mem
sample $PID 60 -file /tmp/lux-sample.txt
kill -TERM $PID
```

### 6.2 空闲 3 分钟实测（默认配置，`#[tokio::main]` 默认 worker 数 = 10，21 线程）

| 窗口（60 s） | 上下文切换增量 | CPU 时间增量 | `%CPU` | `IDLEW` 累计 | RSS |
| --- | --- | --- | --- | --- | --- |
| 第 1 分钟（启动后约 7–8 分钟） | +15 315 | +0.49 s | 0.8 | 281 | 88 MB |
| 第 2 分钟 | +16 593 | +0.37 s | 0.6 | 282 | 68 MB |
| 第 3 分钟 | +16 245 | +0.31 s | 0.5 | 288 | 68 MB |
| **平均** | **≈ +16 051 次/分（≈ 268 次/秒）** | **≈ 0.39 s/分（≈ 0.65 %×1 核）** | 0.5–0.8 | +7/分 | 68–88 MB |

补充测量：

- `TOKIO_WORKER_THREADS=1`（12 线程）：+10 217 次/分；运行 6.5 分钟后的稳态窗口 +9 916 次/分、
  +0.28 s CPU/分、+28 缺页/分。
- `TOKIO_WORKER_THREADS=1 LUX_DB_MAX_CONNECTIONS=1`（6 线程）：+11 446 次/分。
  → 上下文切换量与 worker 数、连接数相关度低，**不是线程池空转导致的**。
- `sample 60 s`：每个线程 45 300 个样本，栈顶分布为 `semaphore_wait_trap` 407 603、
  `__psynch_cvwait` 53 379、`__workq_kernreturn` 45 300、`kevent` 45 087、
  `sqlite3VdbeExec` **5**、tokio `schedule_task` **5**。
  → **没有忙循环**；一分钟内 SQLite 字节码执行时间只有约 5–10 ms。

### 6.3 `IDLEW` 为什么测不出来（工具限制，如实说明）

`top -stats idlew` 在 macOS 上给出的是**累计**值（用 `SIGSTOP` 验证：进程停止 30 s 只 +1，恢复运行
30 s 则 +7 283，说明计数器语义正确、只统计运行期）。但它在**这台开发机**上测不出定时器唤醒：

| 对照进程 | 唤醒频率 | `IDLEW` 30–60 s 增量 |
| --- | --- | --- |
| `python3 -c "sleep(0.5) 循环"` | 2 次/秒 | **0** |
| `node -e "setInterval(()=>{},1000)"` | 1 次/秒 | **0** |
| 空闲 tokio 探针（见 6.5） | 8.2 次/秒 | **0** |

结论：macOS 的 `IDLEW` 只统计"把 CPU 从**深度空闲**唤醒"的次数。本机是开着 Power Nap、跑着多个
并行开发任务的开发机，CPU 包几乎不会进入深度空闲，所以该列对 luxd 也只给出 0–7 次/分的噪声值。
**在 NAS 上（进程是主要活动、系统会真正进入低功耗）用 `powermetrics --samplers tasks` 才能直接读出
per-process idle wakeups——本机 `powermetrics` 需要 root，而 `sudo` 需要密码，无法执行。**

因此本文用以下三个可用指标代替：

1. **CPU 时间/分钟**（`top -stats time,cpu`）：0.28–0.49 s/分，最直接对应"低功耗 CPU 被占用多久"。
2. **上下文切换/分钟**（`top -stats csw`）：先做校准（§6.5），再反推唤醒量级。
3. **文件写入**（`lux.db`、`lux.db-wal`、`lux.db-shm` 的 mtime 与大小）：判断空闲是否真的写盘。

### 6.4 空闲时是否写盘

在稳态窗口前后各取一次文件状态，**一分钟内 `lux.db`（2 383 872 字节）、`lux.db-wal`（0 字节）、
`lux.db-shm`（32 768 字节）的 mtime 与大小完全不变** → 空闲状态**没有写入**，只有读查询。
（SQLite 读查询命中页缓存时不会触碰磁盘，也不改变 mtime。）

### 6.5 上下文切换的校准与反推

为了让 `csw` 能被解释，另做了几组等价对照（不改动仓库代码，全部在 `/tmp` 内构建/运行，测完删除）：

| 探针 | 定时器 | 唤醒/分 | 线程 | `csw`/分 | 每次唤醒的切换数 |
| --- | --- | --- | --- | --- | --- |
| 空闲 tokio 探针（250 ms + 4×1 s + 5 s） | 与 §2.1 前 5 项相同 | **492** | 12 | **376** | **0.76** |
| `node` 单 1 s 定时器 | 1 s | 60 | 8 | 123 | 2.05 |
| `python` 10 线程 × 1 s 定时器 | 1 s×10 | 600 | 11 | 722 | 1.20 |
| `python` 单线程 `sleep(70)` | 无 | ~1 | 1 | 22（全在启动期） | — |

**关键结论：一个定时器唤醒在这台机器上只值 0.76–2 次上下文切换。** 用这个系数反推，luxd 空闲时
≈ 16 051 次切换/分 ⇒ **约 8 000–21 000 次唤醒/分（约 130–350 次/秒）**，是 §5 静态估算
（476 次/分）的 **20–40 倍**。

即使把"每次周期性查询要经过 tokio worker → sqlx worker 线程往返、产生 5–10 次切换"这一放大效应
算进去（980 条查询/分 × 10 ≈ 9 800 次切换/分），也只能解释实测值的 2/3 左右。剩余部分在本任务范围内
**无法归因**：`sample` 抓不到（单次事件只有几微秒），`powermetrics` 需要 root。

这本身是一个需要后续处理的发现：**盘点的 18 个周期循环不足以解释实测的空闲活跃度**，
所以在做"统一调度器"之前，先给后台循环加迭代计数（见 §7 B5）才能验证改造收益。

### 6.6 停止与清理

- `kill -TERM` 后进程 5–6 秒内正常退出（`ps` 无残留），`lsof` 确认 18097/TCP 与 17359/UDP 均已释放。
- 临时目录（配置、空库、日志，共 3.2 MB）已 `rm -rf` 删除，`/tmp` 下无残留；未触碰仓库内 `config/`，
  也未触碰本机其他工作区的 luxd 进程。

### 6.7 实测中观察到的其他事实

- 启动后 5 分钟会跑一次**一次性**只读数据库诊断（`START_DELAY` = 5 min，§2.2-24），日志确认
  `read-only database diagnostics report is ready`。它不是周期性的，但会在启动 5 分钟后集中做一次
  全库元数据查询，属于"启动后延迟重 IO"。
- 空库启动时 `catalog_items` / 用户"下一集"重建各耗时 34 ms / 20 ms（一次性）。
- 空闲 RSS 68–88 MB，线程 21 个（10 tokio worker + 8 sqlite worker + 主线程 + 日志写入 + 其他）。

---

## 7. 分级建议

收益按 §5.2 的静态估算口径给出（默认配置、空闲）。

### A 类：零风险，直接改（事件驱动或加长周期）

| 编号 | 对象 | 改动 | 预期收益 | 风险 |
| --- | --- | --- | --- | --- |
| **A1** | 本地元数据 outbox（`scanner/jobs/local_metadata.rs:1053`） | 空闲兜底 250 ms → 5–10 s，保留 `Notify` 快路径 | **−234 次/分**（占总量 −49 %），并减少约 470 条空闲查询/分 | 极低。进程内入队已有 `notify_one`（`:1346`），只有"其它路径/其它进程直接写 outbox 表"的行最多延迟 10 s 被拾取。需确认没有依赖"250 ms 内必然被拾取"的测试 |
| **A2** | `catalog_items` 增量 + 搜索重建索引（`catalog_read_model.rs:158`、`:226`） | 1 s → 5 s，或改为写侧 `Notify`（同进程写的 outbox） | **−96 次/分**，−240 条查询/分 | 低。读模型/搜索索引新鲜度从 ~1 s 变 ~5 s；代码注释自己写的是"一个 tick 内可见"，改成 5 s 只是把语义放宽，不影响正确性 |
| **A3** | 全局合集同步（`collections.rs:233`） | 1 s → 5 s，并复用已有 `watch` 加 `Notify` | **−48 次/分**，−96 条查询/分 | 低。合集元数据延迟 5 s；无正确性依赖 |
| **A4** | Web 播放会话回收（`playback/session.rs:241`） | 5 s → 由会话心跳/停止事件驱动，保留 30 s 兜底 | **−10 次/分** | 中低。正在播放的客户端心跳会驱动它；兜底 30 s 意味着异常断开的会话最多多占 30 s 配额 |
| **A5** | 邮件 / webhook 轮询（`mail_worker.rs:135`、`webhooks.rs:170`） | 入队时 `notify`，兜底 5 s → 60 s | **−22 次/分** | 低。两个入队点都在进程内、都可加 notify；兜底 60 s 只影响"跨进程入队"的场景（当前不存在） |
| **A6** | 用户清理（`user_cleanup.rs:33`） | 60 s → 3600 s | −1 次/分（但每次 3 条查询） | 极低。保留期是天级，分钟级探测没有意义 |
| **A7** | 扫描指纹迁移（`scan_fingerprint_migration.rs:50`） | 所有库 `completed_at` 齐了之后**退出循环**；未完成时空闲间隔 10 s → 60 s | −6 次/分，并停止每 10 s 的库列表查询 | 低。新建媒体库时需要一次显式唤醒（可由库创建路径发 `notify`），否则要等下一轮 |

**A 类全部实施后：空闲唤醒 ≈ 476 → 约 60 次/分（−87 %），空闲查询 ≈ 980 → 约 130 条/分（−87 %）。**
且 A1–A3 是纯参数/通知改动，不改变任何数据语义。

### B 类：需要设计

| 编号 | 对象 | 需要设计什么 | 预期收益 | 风险 |
| --- | --- | --- | --- | --- |
| **B1** | 计划任务调度（`scheduled_tasks.rs:167`，15 s 轮询） | 改为"睡到下一个计划触发时刻"的单次定时器：需要计算下一次 `cron`/`interval` 触发、处理计划增删改后的重算、时钟跳变/DST、以及 `claim_minute` 的幂等性 | −4 次/分 + 每 15 s 的 3 条查询全部消失（空闲时接近 0） | 中。调度正确性风险最高（漏跑/重跑）；需要覆盖时钟回拨、跨分钟边界、关闭-重启的测试 |
| **B2** | 8 个无 `JoinHandle` 的常驻循环（§2.3） | 统一改造成 `background_loop::BackgroundLoop`（`watch` 信号 + 受监督重启），并纳入 `shutdown_background_workers_checked` 的排空序列 | 不影响空闲唤醒；消除"停机过程中仍在 claim/写库"的窗口 | 中。要确定排空顺序（先停生产者再生者），且 `serve()` 的退出路径需要相应调整 |
| **B3** | 收敛到统一调度器（**本任务不决策**） | 一个 tick 源 + 每任务登记"下次运行时刻"，把 N 个定时器合成 1 个；需要任务优先级、饥饿保护、可观测性、超时与取消 | 理论上可把空闲唤醒压到每分钟个位数（取决于最小公共周期） | 高。单点故障、任务饥饿、排障困难、"一个 panic 拖垮全部后台任务"的爆炸半径；建议在 A 类全部落地、并完成 B5 的度量之后再评估 |
| **B4** | 跨进程/跨路径 outbox 唤醒策略（A1–A3 的前提） | 明确 outbox 的写入方是否可能在不同进程；若是，需要 DB 级通知（PostgreSQL `LISTEN/NOTIFY`）或低频兜底 SLA（例如"最迟 10 s"） | 决定 A 类能压到多低 | 低–中。要有明确的新鲜度 SLA，否则兜底周期无法确定 |
| **B5** | 给后台循环加**迭代计数器** | 用现有 `tracing`/指标暴露每个循环的迭代次数（例如每 60 s 汇总一次），或在 `/api/v1/admin/performance` 里加一节 | 不省电，但能解释 §6.5 的 20–40 倍差异，并为 A/B 类改造提供回归基线 | 低。只是新增观测点 |

### C 类：保持现状

| 编号 | 对象 | 理由 |
| --- | --- | --- |
| **C1** | SSE 心跳（`admin_logs_events.rs:380`/`:447`，15 s） | 只在有客户端连接时存在，是 SSE 协议的保活需要；不在空闲路径上 |
| **C2** | 探测准入 1 s、STRM 探测启动等待 500 ms、元数据作业页 5 s、HLS 会话循环 | 工作驱动，随作业/会话结束即退出，不影响空闲功耗 |
| **C3** | 数据库维护 24 h、任务历史保留 24 h、查询统计 15 min | 已是刻钟/天级，且是重 IO 低频任务；查询统计间隔由 `LUX_DATABASE_MAINTENANCE` 控制，可关 |
| **C4** | LAN 发现 UDP、`provider_cache` 落盘、`actor_enrichment` 队列、探测排空 | 已经是纯事件驱动（socket / `Notify` / `mpsc` / 条件变量），没有改进空间 |
| **C5** | 身份合并 120 s、游离状态重挂 300 s、登录背景图 300 s | 单项收益 < 1 次/分，改动的风险收益比不划算；若后续做 B3 顺手收编即可 |

---

## 8. 结论

1. 空闲唤醒高度集中：**4 个循环（250 ms outbox + 三个 1 s 消费者）占 88 %**。这 4 个都是"纯轮询的
   outbox/同步消费者"，且其中 2 个已经具备 in-process `Notify`，改成事件驱动在结构上可行。
2. 静态估算的空闲唤醒是 **476 次/分**，空闲查询约 **980 条/分**；实测（debug 构建、M4）空闲 CPU 为
   **0.28–0.49 s/分（0.5–0.8 % 单核）**，一分钟内**零写入**。
3. 用上下文切换校准后反推，实测空闲活跃度是静态估算的 **20–40 倍**，其中约 2/3 可以用"每次查询的
   线程往返放大"解释，剩余部分无法归因——这需要 B5 的迭代计数器或 NAS 上的 `powermetrics`（root）
   才能定论。
4. `top` 的 `IDLEW` 在这台开发机上不可用（深度空闲不发生），已用 `SIGSTOP` 验证计数器语义并用对照
   进程证明该列恒为 0；因此本文改用 CPU 时间 + 上下文切换（含校准）+ 文件写入三个可复现指标。
5. 建议顺序：**先做 A 类（零风险、可独立提交、合计 −87 % 空闲唤醒），再决定 B1/B2**；
   B3（统一调度器）应在 A 类完成且 B5 提供了度量之后再评估。
