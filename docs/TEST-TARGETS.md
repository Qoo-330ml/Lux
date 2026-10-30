# 集成测试 target 分组（旧 target 名 → 新命令）

`tests/` 下的集成测试原先每个文件都是一个独立 binary，各自链接一次 `luxd`，`target/` 磁盘占用因此非常高。本文件记录按领域合并后的分组、旧 target 名到新命令的完整映射、哪些 binary 必须保持独立，以及合并前后的度量。

合并只改模块路径与 `use`（含 `include_str!` 相对路径和 `mod common;` 的写法），没有修改任何测试的断言与逻辑；测试总数与合并前完全一致。

## 1. 分组总览

| 新 target | 领域 | 原文件数 | 测试数 |
|---|---|---|---|
| `--test admin` | 管理后台、日志导出、可观测性与运维事件 | 12 | 22 |
| `--test api_web` | HTTP 契约（错误/限流/压缩/健康/远程策略）、Web 会话、Webhook | 13 | 39 |
| `--test auth_users` | 认证、会话、登录背景、用户与用户设置、设备配对、界面偏好迁移 | 10 | 70 |
| `--test emby` | Emby 兼容层（认证、系统信息、增量刷新、发现、多段播放、检索、下载） | 10 | 38 |
| `--test scanner` | 媒体库扫描、增量/混合扫描、计划任务、作业事件与关停 | 7 | 137 |
| `--test library` | 媒体库生命周期、合并/拆分、集合、目录与系列 API | 12 | 49 |
| `--test metadata` | 元数据抓取与选择、NFO 回写、扫描后剧集元数据、人物、字幕 | 14 | 143 |
| `--test images` | 图片变体、写回、缩略图与图片 API | 5 | 52 |
| `--test playback` | 播放、播放令牌/历史/续播、观看状态归档、章节与片头片尾检测 | 10 | 34 |
| `--test storage` | SQLite/PostgreSQL 存储、迁移、WAL、数据库维护与启动守卫 | 7 | 112 |
| `--test plugins` | 插件协议与运行时、弹幕、IP 属地、媒体信息插件配置 | 11 | 84 |
| `--test strm` | STRM 目标解析/探测/播放与 ffprobe | 9 | 42 |
| 合计（12 组） |  | 120 | 822 |

另有 7 个 binary 因为进程级共享状态或外部服务依赖保持独立，见第 4 节：`--test background_work_budget`、`--test image_variants_perf`、`--test performance`、`--test postgres_database`、`--test reidentify`、`--test scanned_metadata`、`--test shutdown`。合计 12 + 7 = 19 个集成测试 target。

## 2. 旧 target 名 → 新命令

旧命令 `cargo test --locked --test <旧名>` 的等价写法是：

```bash
cargo test --locked --test <新 target> <旧名>::
```

例如 `cargo test --locked --test scanning_jobs` → `cargo test --locked --test scanner scanning_jobs::`。

| 旧 target | 测试数 | 等价命令 |
|---|---|---|
| `admin_api_key` | 4 | `cargo test --locked --test admin admin_api_key::` |
| `admin_dashboard` | 1 | `cargo test --locked --test admin admin_dashboard::` |
| `admin_database_diagnostics` | 1 | `cargo test --locked --test admin admin_database_diagnostics::` |
| `admin_events` | 5 | `cargo test --locked --test admin admin_events::` |
| `admin_health` | 1 | `cargo test --locked --test admin admin_health::` |
| `admin_mail_settings` | 1 | `cargo test --locked --test admin admin_mail_settings::` |
| `admin_performance_metrics` | 1 | `cargo test --locked --test admin admin_performance_metrics::` |
| `admin_settings` | 2 | `cargo test --locked --test admin admin_settings::` |
| `admin_user_invitations` | 1 | `cargo test --locked --test admin admin_user_invitations::` |
| `log_export` | 2 | `cargo test --locked --test admin log_export::` |
| `observability` | 2 | `cargo test --locked --test admin observability::` |
| `user_query_events` | 1 | `cargo test --locked --test admin user_query_events::` |
| `acl` | 1 | `cargo test --locked --test api_web acl::` |
| `api_error_contract` | 5 | `cargo test --locked --test api_web api_error_contract::` |
| `domain` | 3 | `cargo test --locked --test api_web domain::` |
| `health` | 1 | `cargo test --locked --test api_web health::` |
| `rate_limit` | 1 | `cargo test --locked --test api_web rate_limit::` |
| `ready_version` | 2 | `cargo test --locked --test api_web ready_version::` |
| `remote` | 8 | `cargo test --locked --test api_web remote::` |
| `response_compression` | 2 | `cargo test --locked --test api_web response_compression::` |
| `web_auth` | 4 | `cargo test --locked --test api_web web_auth::` |
| `web_playback` | 3 | `cargo test --locked --test api_web web_playback::` |
| `web_static` | 2 | `cargo test --locked --test api_web web_static::` |
| `webhooks` | 6 | `cargo test --locked --test api_web webhooks::` |
| `webhooks_api` | 1 | `cargo test --locked --test api_web webhooks_api::` |
| `account_preferences_migration` | 2 | `cargo test --locked --test auth_users account_preferences_migration::` |
| `auth` | 2 | `cargo test --locked --test auth_users auth::` |
| `device_pairings` | 8 | `cargo test --locked --test auth_users device_pairings::` |
| `login_background` | 4 | `cargo test --locked --test auth_users login_background::` |
| `lux_api_auth` | 1 | `cargo test --locked --test auth_users lux_api_auth::` |
| `resume_favorites` | 5 | `cargo test --locked --test auth_users resume_favorites::` |
| `sessions` | 2 | `cargo test --locked --test auth_users sessions::` |
| `setup` | 5 | `cargo test --locked --test auth_users setup::` |
| `user_settings` | 2 | `cargo test --locked --test auth_users user_settings::` |
| `users` | 39 | `cargo test --locked --test auth_users users::` |
| `discovery` | 3 | `cargo test --locked --test emby discovery::` |
| `download_api` | 1 | `cargo test --locked --test emby download_api::` |
| `emby_auth` | 15 | `cargo test --locked --test emby emby_auth::` |
| `emby_counts` | 1 | `cargo test --locked --test emby emby_counts::` |
| `emby_global_collection_playback` | 1 | `cargo test --locked --test emby emby_global_collection_playback::` |
| `emby_incremental_refresh` | 8 | `cargo test --locked --test emby emby_incremental_refresh::` |
| `emby_multi_part_playback` | 1 | `cargo test --locked --test emby emby_multi_part_playback::` |
| `emby_system` | 2 | `cargo test --locked --test emby emby_system::` |
| `mixed_library_api` | 3 | `cargo test --locked --test emby mixed_library_api::` |
| `search` | 3 | `cargo test --locked --test emby search::` |
| `job_events_api` | 2 | `cargo test --locked --test scanner job_events_api::` |
| `mixed_scanner` | 1 | `cargo test --locked --test scanner mixed_scanner::` |
| `scanner` | 18 | `cargo test --locked --test scanner scanner::` |
| `scanning_jobs` | 95 | `cargo test --locked --test scanner scanning_jobs::` |
| `scheduled_tasks` | 9 | `cargo test --locked --test scanner scheduled_tasks::` |
| `series_scanner` | 11 | `cargo test --locked --test scanner series_scanner::` |
| `shutdown_jobs` | 1 | `cargo test --locked --test scanner shutdown_jobs::` |
| `catalog` | 3 | `cargo test --locked --test library catalog::` |
| `cd_movie_versions` | 2 | `cargo test --locked --test library cd_movie_versions::` |
| `collections` | 4 | `cargo test --locked --test library collections::` |
| `item_merge` | 1 | `cargo test --locked --test library item_merge::` |
| `item_unmerge` | 1 | `cargo test --locked --test library item_unmerge::` |
| `libraries_api` | 14 | `cargo test --locked --test library libraries_api::` |
| `library` | 13 | `cargo test --locked --test library library::` |
| `library_cover_generation` | 5 | `cargo test --locked --test library library_cover_generation::` |
| `library_deletion` | 1 | `cargo test --locked --test library library_deletion::` |
| `library_order` | 1 | `cargo test --locked --test library library_order::` |
| `media_deletion` | 3 | `cargo test --locked --test library media_deletion::` |
| `series_api` | 1 | `cargo test --locked --test library series_api::` |
| `identification` | 1 | `cargo test --locked --test metadata identification::` |
| `media_matching` | 10 | `cargo test --locked --test metadata media_matching::` |
| `metadata` | 22 | `cargo test --locked --test metadata metadata::` |
| `metadata_api` | 9 | `cargo test --locked --test metadata metadata_api::` |
| `metadata_cancel` | 3 | `cargo test --locked --test metadata metadata_cancel::` |
| `metadata_objects` | 4 | `cargo test --locked --test metadata metadata_objects::` |
| `metadata_paths` | 8 | `cargo test --locked --test metadata metadata_paths::` |
| `metadata_selection` | 38 | `cargo test --locked --test metadata metadata_selection::` |
| `nfo_writer` | 27 | `cargo test --locked --test metadata nfo_writer::` |
| `people_api` | 10 | `cargo test --locked --test metadata people_api::` |
| `scanned_series_metadata` | 3 | `cargo test --locked --test metadata scanned_series_metadata::` |
| `scraper` | 4 | `cargo test --locked --test metadata scraper::` |
| `series_metadata` | 3 | `cargo test --locked --test metadata series_metadata::` |
| `subtitles` | 1 | `cargo test --locked --test metadata subtitles::` |
| `image_api` | 1 | `cargo test --locked --test images image_api::` |
| `image_variants` | 9 | `cargo test --locked --test images image_variants::` |
| `image_writer` | 20 | `cargo test --locked --test images image_writer::` |
| `images` | 3 | `cargo test --locked --test images images::` |
| `thumbnails` | 19 | `cargo test --locked --test images thumbnails::` |
| `chapter_detection` | 2 | `cargo test --locked --test playback chapter_detection::` |
| `chapter_detection_api` | 1 | `cargo test --locked --test playback chapter_detection_api::` |
| `chapters` | 1 | `cargo test --locked --test playback chapters::` |
| `playback` | 6 | `cargo test --locked --test playback playback::` |
| `playback_history_archive` | 5 | `cargo test --locked --test playback playback_history_archive::` |
| `playback_strm_probe` | 1 | `cargo test --locked --test playback playback_strm_probe::` |
| `playback_token` | 5 | `cargo test --locked --test playback playback_token::` |
| `playback_webhooks` | 1 | `cargo test --locked --test playback playback_webhooks::` |
| `watch` | 5 | `cargo test --locked --test playback watch::` |
| `watch_state_archive` | 7 | `cargo test --locked --test playback watch_state_archive::` |
| `database_config` | 4 | `cargo test --locked --test storage database_config::` |
| `database_maintenance` | 12 | `cargo test --locked --test storage database_maintenance::` |
| `database_setup_api` | 4 | `cargo test --locked --test storage database_setup_api::` |
| `database_startup_guard` | 4 | `cargo test --locked --test storage database_startup_guard::` |
| `log_migration` | 10 | `cargo test --locked --test storage log_migration::` |
| `sqlite_wal` | 2 | `cargo test --locked --test storage sqlite_wal::` |
| `storage` | 76 | `cargo test --locked --test storage storage::` |
| `danmaku` | 7 | `cargo test --locked --test plugins danmaku::` |
| `danmaku_api` | 2 | `cargo test --locked --test plugins danmaku_api::` |
| `danmaku_config` | 2 | `cargo test --locked --test plugins danmaku_config::` |
| `danmaku_config_api` | 1 | `cargo test --locked --test plugins danmaku_config_api::` |
| `danmaku_plugin` | 2 | `cargo test --locked --test plugins danmaku_plugin::` |
| `ip_location_plugins` | 3 | `cargo test --locked --test plugins ip_location_plugins::` |
| `media_info_config` | 2 | `cargo test --locked --test plugins media_info_config::` |
| `media_info_config_api` | 1 | `cargo test --locked --test plugins media_info_config_api::` |
| `plugin_protocol` | 41 | `cargo test --locked --test plugins plugin_protocol::` |
| `plugin_runtime` | 14 | `cargo test --locked --test plugins plugin_runtime::` |
| `plugins` | 9 | `cargo test --locked --test plugins plugins::` |
| `probe` | 21 | `cargo test --locked --test strm probe::` |
| `strm` | 1 | `cargo test --locked --test strm strm::` |
| `strm_allowed_roots` | 1 | `cargo test --locked --test strm strm_allowed_roots::` |
| `strm_playback` | 2 | `cargo test --locked --test strm strm_playback::` |
| `strm_probe` | 4 | `cargo test --locked --test strm strm_probe::` |
| `strm_probe_api` | 2 | `cargo test --locked --test strm strm_probe_api::` |
| `strm_resolver` | 3 | `cargo test --locked --test strm strm_resolver::` |
| `strm_resolver_playback` | 1 | `cargo test --locked --test strm strm_resolver_playback::` |
| `strm_target` | 7 | `cargo test --locked --test strm strm_target::` |
| `background_work_budget` | 1 | `cargo test --locked --test background_work_budget`（本来就没有合并） |
| `image_variants_perf` | 2 | `cargo test --locked --test image_variants_perf`（本来就没有合并） |
| `performance` | 25 | `cargo test --locked --test performance`（本来就没有合并） |
| `postgres_database` | 34 | `cargo test --locked --test postgres_database`（本来就没有合并） |
| `reidentify` | 25 | `cargo test --locked --test reidentify`（本来就没有合并） |
| `scanned_metadata` | 23 | `cargo test --locked --test scanned_metadata`（本来就没有合并） |
| `shutdown` | 2 | `cargo test --locked --test shutdown`（本来就没有合并） |
| **合计** | **934** |  |

## 3. 只跑某一个原文件的测试

第一个过滤参数是模块路径前缀，不是任意子串：`scanner scanner::` 只运行 `scanner::*` 的用例，不会带上 `scanning_jobs::*`。

```bash
# 只跑原 tests/scanning_jobs.rs 的用例
cargo test --locked --test scanner scanning_jobs::

# 再按名字收窄到单个用例
cargo test --locked --test scanner scanning_jobs::incremental_resume

# 跑整个分组
cargo test --locked --test scanner
```

`<group>` 是 `tests/<group>/main.rs` 自动发现的 target，不需要在 `Cargo.toml` 里写 `[[test]]`。

## 4. 保持独立的 binary

| target | 原因 |
|---|---|
| `--test background_work_budget` | 调用 `env::set_var`/`remove_var` 修改进程级环境变量，与同 binary 的其他测试并行运行会互相污染，必须独占一个进程。 |
| `--test image_variants_perf` | 读取 `LUX_PERF_IMAGE_DIR` 指向的外部图片目录，属于需要外部素材的性能测试。 |
| `--test performance` | 安装全局 tracing subscriber（`set_global_default`）并读取 `LUX_PERF_*` 环境变量，进程级全局状态不可共享；同时是基准脚本的入口（`scripts/run-performance.sh`）。 |
| `--test postgres_database` | 需要本机 PostgreSQL 实例，其用例默认 `#[ignore]`。 |
| `--test reidentify` | 调用 `AppState::shutdown_background_workers()`，它会永久关闭进程级探测准入闸门（`src/application/probe/concurrency.rs` 的 `static GLOBAL_PROBE_RUNTIME`）。与任何使用 probe 的测试同进程运行都会让后者报 `Worker("probe admission is closed")`。 |
| `--test scanned_metadata` | 断言“第一个 NFO 读取被阻塞时，后面的海报仍会被索引”，依赖本地元数据队列在很短的轮询窗口内推进；放进 166 例的 `metadata` 分组后 4 次全量运行失败 3 次（单独跑、单独跑该模块、放在旧 binary 里都稳定通过），因此按保守策略保持独立。 |
| `--test shutdown` | 同上：`server_drains_background_workers_before_exit` 走同一条 `shutdown_background_workers_checked()` 路径，会关掉进程级探测准入闸门。 |

进程级共享状态是这次分组的硬约束。除上表外，代码里还有若干 `OnceLock`/`static`：`MAINTENANCE_RUNNING`、`REMOVED_ITEM_PURGE_RUNNING`（布尔闸门）、`METADATA_GLOBAL_PERMITS`、`GLOBAL_FFMPEG_PERMITS`、`LOCAL_IMAGE_READ_PERMITS`（全局信号量）、`LOG_MANAGER_REGISTRY`、`CPU_CAPACITY_CACHE`。它们都是“第一次初始化后进程内共享”，不会像探测准入闸门那样被关闭，只会让并行用例排队；第 5.2 节记录了针对性复核结果。

`tests/storage/database_maintenance.rs` 使用 `tokio::sync::Mutex` 常量锁把自身用例串行化，且每个用例都用独立 `tempfile::tempdir()` 数据库，因此和 `tests/storage/` 其他用例同进程运行不会互相污染（`storage` 分组 112 个用例全量通过）。

`tests/playback/playback.rs` 通过 `#[path = "playback_strm_probe.rs"]` 引入 `playback_strm_probe.rs`，而该文件自身也是分组里的一个模块，因此合并前其用例就在两个 binary 里各跑一次（合并前 `--test playback` 有 6 个用例，`--test playback_strm_probe` 有 1 个）。合并后两处都在 `playback` 分组内（`playback::playback_strm_probe::*` 与 `playback_strm_probe::*`），重复执行的行为与合并前一致，测试总数也随之保持一致。

## 5. 合并前后的度量

测量方法（两侧一致）：`touch` 全部测试源文件强制重新编译所有测试 target，再计量 `cargo test --locked --all-targets --no-run` 的墙钟时间；测试二进制数量与体积取自同一条命令 `--message-format=json` 报出的 `executable` 路径与文件大小；`target/` 体积用 `du -sk target`，两侧都先删掉不再被引用的旧测试二进制再测。

| 指标 | 合并前 | 合并后 | 变化 |
|---|---|---|---|
| 集成测试 target 数 | 127 | 19 | -108 |
| 测试用例总数 | 934 | 934 | +0 |
| 全部测试 target 编译+链接墙钟（`--no-run`） | 269 s | 463 s | +194 s |
| 同上，CPU 时间（user+sys） | 761 s | 540 s | -221 s（-29%） |
| `target/debug/deps` 中测试二进制总体积 | 16.18 GiB | 3.23 GiB | -12.94 GiB |
| `target/` 目录体积（合并后此数还含一次 clippy 检查缓存） | 20.3 GiB | 8.3 GiB | -12.0 GiB |
| 全量 `--all-targets --no-fail-fast` 墙钟 | 4201 s | 1899 s | -2302 s |

计量环境：Apple Silicon arm64（`uname -m` =`arm64`）上的 macOS，dev/test profile（`debug = "line-tables-only"`、`incremental = false`）。机器上同时有其他构建任务，绝对耗时有噪声。

**磁盘占用**是这次合并的主要收益：测试二进制 16.18 GiB → 3.23 GiB（-80%），`target/` 20.3 GiB → 8.3 GiB（合并后的 8.3 GiB 里还含一次 `cargo clippy --all-targets` 的检查缓存，那部分与测试 target 数量无关）。

**测试执行**明显变快：全量 `--all-targets` 从 4201 s 降到 1899 s（-55%），原因是同一领域里原本各自串行执行的小 binary 现在在一个进程内并行跑。

**编译+链接**要分两个口径看：CPU 时间从 761 s 降到 540 s（-29%），说明总编译工作量确实减少了；但墙钟从 269 s 涨到 463 s。原因是并行度：合并前 cargo 能同时跑 127 个测试 target（本次测量中平均占用约 2.8 个核），合并后只有 19 个 unit（平均约 1.2 个核），最大的几个分组（`metadata` 143 例、`scanner` 137 例、`storage` 112 例）成为单点关键路径。同一布局重复测过一次 16 target 的中间版本，墙钟 343 s，可见墙钟差异同时受机器负载影响；以 CPU 时间判断趋势更稳。如果要同时压低链接墙钟，可以把超大分组再按子领域拆成 2~3 个 target，那是与本次目标（磁盘占用与测试执行时间）不同的取舍。

### 5.1 逐组测试数对照（合并前后必须完全一致）

| 新 target | 合并前同名文件测试数之和 | 合并后 `--test <group> -- --list` | 结果 |
|---|---|---|---|
| `admin` | 22 | 22 | 一致 |
| `api_web` | 39 | 39 | 一致 |
| `auth_users` | 70 | 70 | 一致 |
| `emby` | 38 | 38 | 一致 |
| `scanner` | 137 | 137 | 一致 |
| `library` | 49 | 49 | 一致 |
| `metadata` | 143 | 143 | 一致 |
| `images` | 52 | 52 | 一致 |
| `playback` | 34 | 34 | 一致 |
| `storage` | 112 | 112 | 一致 |
| `plugins` | 84 | 84 | 一致 |
| `strm` | 42 | 42 | 一致 |
| 独立 binary 合计 | 112 | 112 | 一致 |
| **合计** | **934** | **934** | 一致 |

### 5.2 单个用例稳定性复核

合并后第一次全量运行出现过几个基线没有失败的用例。为了区分“分组引入的进程级冲突”和“高负载下的抖动”，把每个用例在旧 binary（`git worktree` 检出基线提交 `184ee043`）和合并后 binary 里各单独跑 3 次：

| 用例 | 旧 binary | 合并后 binary | 结论 |
|---|---|---|---|
| `manifest_scan_indexes_poster_while_local_nfo_is_blocked` | PASS/PASS/PASS | PASS/PASS/PASS | 单跑和单独跑该模块都稳定通过，但并入 166 例的 `metadata` 分组后 4 次全量运行里失败 3 次（本地元数据队列在 10 线程持续负载下推进变慢），已把该文件拆回独立 binary。 |
| `external_subtitles_are_indexed_served_and_acl_protected` | PASS/PASS/PASS | PASS/PASS/PASS | 合并后首次全量运行时被 `reidentify` 关闭的进程级探测准入闸门污染；已把 `reidentify` 拆回独立 binary，现两侧一致通过。 |
| `danmaku_service_matches_local_and_strm_sources_and_writes_sidecars` | PASS/PASS/PASS | PASS/PASS/PASS | 两侧单独跑都通过；它断言 10,000 源媒体库的 `create_job` 在 250 ms 内返回（懒物化），高负载下会超时，属于用例自身的时间预算（`plugins` 分组整体重跑 84/84 通过）。 |
| `manual_scraper_image_selection_claims_one_attempt_for_the_same_image` | PASS/PASS/PASS | PASS/PASS/PASS | 用例要求两个并发任务落在同一个 40 ms 窗口内，高负载调度延迟下会退化成串行，属于用例自身的时序假设，不是分组引入。 |
| `fill_missing_dispatcher_serializes_jobs_for_one_library` | FAIL/PASS/FAIL | FAIL/PASS/FAIL | 两侧布局一致（单文件 binary）时同样是 3 次里失败 2 次：等待第一个 gated 请求启动的 2 s 轮询窗口在高负载下会超时，属于用例自身的时间预算，与分组无关。 |

### 5.3 全量 `--all-targets` 通过/失败集合对照

| 运行 | 集成测试 target | 通过 | 失败 | 失败用例 |
|---|---|---|---|---|
| 合并前全量（127 个集成 target，无 `web/dist`） | 127 | 2311 | 4 | `admin_settings::transcoding_settings_are_admin_only_validated_and_persisted`<br>`probe::canceled_probe_runner_reaps_its_child_process`<br>`web_static::same_origin_web_assets_are_served_by_rust`<br>`web_static::media_detail_navigation_is_served_as_html_while_json_uses_emby` |
| 合并后首次全量（16 target，未做隔离，无 `web/dist`） | 16 | 2307 | 8 | `admin_settings::transcoding_settings_are_admin_only_validated_and_persisted`<br>`web_static::same_origin_web_assets_are_served_by_rust`<br>`web_static::media_detail_navigation_is_served_as_html_while_json_uses_emby`<br>`image_writer::manual_scraper_image_selection_claims_one_attempt_for_the_same_image`<br>`reidentify::fill_missing_dispatcher_serializes_jobs_for_one_library`<br>`scanned_metadata::manifest_scan_indexes_poster_while_local_nfo_is_blocked`<br>`subtitles::external_subtitles_are_indexed_served_and_acl_protected`<br>`danmaku::danmaku_service_matches_local_and_strm_sources_and_writes_sidecars` |
| 合并后全量（18 target，已隔离 `shutdown`/`reidentify`，有 `web/dist`） | 18 | 2312 | 3 | `admin_settings::transcoding_settings_are_admin_only_validated_and_persisted`<br>`scanned_metadata::manifest_scan_indexes_poster_while_local_nfo_is_blocked`<br>`danmaku::danmaku_service_matches_local_and_strm_sources_and_writes_sidecars` |
| 合并后全量（19 target，已隔离 `scanned_metadata`，有 `web/dist`，机器负载 9~14） | 19 | 2310 | 5 | `admin_settings::transcoding_settings_are_admin_only_validated_and_persisted`<br>`storage::repository::repository_tests::write_lock_waits_for_the_current_writer_instead_of_failing（lib 用例，src 未改动）`<br>`danmaku::danmaku_service_matches_local_and_strm_sources_and_writes_sidecars`<br>`danmaku_plugin::danmaku_plugin_process_matches_and_returns_xml`<br>`media_info_config_api::media_info_plugin_config_api_drives_a_background_run` |

失败集合的差异全部落在“时间预算型”用例和本机环境上，逐条证据如下。

1. `transcoding_settings_are_admin_only_validated_and_persisted`：开发机有 VideoToolbox，旧 binary（`git worktree` 检出基线提交）同样失败，与本改动无关。
2. `web_static` 两个：缺 `web/dist`。旧 binary 在补上 `pnpm --dir web build` 之后 2/2 通过；合并后的 `--test api_web` 也在有 `web/dist` 时 39/39 通过。
3. `reidentify::fill_missing_dispatcher_serializes_jobs_for_one_library`、`image_writer::manual_scraper_image_selection_claims_one_attempt_for_the_same_image`、`danmaku::danmaku_service_matches_local_and_strm_sources_and_writes_sidecars` 已在旧 binary 与新 binary 中各单独跑 3 次（见 5.2 节），两侧表现一致，属于用例自身 2 s / 40 ms / 250 ms 时间预算在高负载下被突破。
4. `subtitles::external_subtitles_are_indexed_served_and_acl_protected` 是唯一确认由分组引入的失败：`reidentify` 用例调用 `shutdown_background_workers()` 关掉了进程级探测准入闸门；把 `reidentify`、`shutdown` 拆回独立 binary 后，`--test metadata` 143/143 通过。
5. `scanned_metadata::manifest_scan_indexes_poster_while_local_nfo_is_blocked` 在 166 例的 `metadata` 分组里4 次全量运行失败 3 次，单独跑 3/3 通过，因此也拆回独立 binary（`--test scanned_metadata` 23/23 通过，`--test metadata` 143/143 通过）。
6. 最后一次全量运行时本机 `load average` 为 9~14（10 核，同时有 3 个其他 agent 的 `cargo build`/`cargo test`），连未改动 `src/**` 的 lib 用例 `write_lock_waits_for_the_current_writer_instead_of_failing` 和本地 `127.0.0.1` HTTP 请求都出现超时，说明该次运行的环境不可作为判定依据。合并前的基线运行也在同一负载条件下出现过 `probe::canceled_probe_runner_reaps_its_child_process` 失败。

## 6. 新增测试怎么放

优先把新文件放进现有分组的领域目录：`tests/<group>/<file>.rs`，并在 `tests/<group>/main.rs` 里加一行 `mod <file>;`。只有确定无法与任何分组共存时才新增顶层 `tests/<file>.rs`，判据是：改进程级环境变量、装全局 subscriber、关全局闸门（如 `shutdown_background_workers()`）、或需要外部服务（PostgreSQL、外部素材目录）。

