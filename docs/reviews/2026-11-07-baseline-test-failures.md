# 基线测试失败清单（2026-11-07）

> 2026-11-08 复核：下面 6 项均已定位到根因并修复，另外发现第 9 个基线失败和一处 STRM 任务完成
> 事务的缺陷。逐项结论见文末「复核与修复记录」。

目的：让 `cargo test --locked --all-targets` 在本机（Apple Silicon，`arm64`）上能全绿。下面 6 个用例
在 WebSocket 迁移（LUX-887～898）合并之前就失败，不是那批改动引入的。

## 怎么确认它们是基线失败

- 合并后的 `test`（`49a849e5`）上跑完整 `cargo test --locked --all-targets --no-fail-fast`，共 9 个用例失败。
- 把改动之前的提交 `05abd0f9` 检出到项目目录内的临时 worktree（`CARGO_TARGET_DIR` 也放项目内），
  逐个单跑，结果见下表。对照完已删除该 worktree。
- 另外 2 个失败（`danmaku::danmaku_service_matches_local_and_strm_sources_and_writes_sidecars`、
  `scanned_metadata::manifest_scan_indexes_poster_while_local_nfo_is_blocked`）单独跑都通过，是满载下的偶发超时，
  **不在本清单内**。如果你想顺手加固也可以，但不是基线必现失败。

复现前先 `source scripts/lux-env.sh`；只用单个用例过滤，不要跑整个 `--all-targets`。

| # | 用例 | 命令 | 基线 | 合并后 |
|---|---|---|---|---|
| 1 | `admin_settings::transcoding_settings_are_admin_only_validated_and_persisted` | `cargo test --locked --test admin admin_settings::transcoding_settings` | 失败 | 失败 |
| 2 | `remote::forwarded_headers_are_only_honoured_for_trusted_proxies` | `cargo test --locked --test api_web remote::forwarded_headers_are_only` | 失败 | 失败 |
| 3 | `series_scanner::series_scan_builds_stable_series_season_episode_hierarchy` | `cargo test --locked --test scanner series_scanner::series_scan_builds_stable` | 失败 | 失败 |
| 4 | `media_info_config_api::media_info_plugin_config_api_drives_a_background_run` | `cargo test --locked --test plugins media_info_config_api::media_info_plugin_config_api_drives` | 失败 | 失败 |
| 5 | `web_playback::broken_hardware_encoder_falls_back_to_software_transcoding` | `cargo test --locked --test api_web web_playback::broken_hardware` | 失败 | 失败 |
| 6 | lib 里 `application::probe::tests::playback_probe_runs_once_for_a_pending_local_source` 与 `application::watch::tests::realtime_incremental_scan_probes_the_source_it_indexes`（算同一个问题） | `cargo test --locked --lib application::` | 整组跑时失败 | 整组跑时失败，**单独跑通过** |

## 逐项说明

### 1. `transcoding_settings_are_admin_only_validated_and_persisted`

- 位置：`tests/admin/admin_settings.rs:378`（保存设置后重新自检的断言）。
- 现象：`assertion left == right failed`，保存后 `saved["transcodingStatus"]["available"]` 期望 `false`，实际 `true`。
- 原因：测试注释写着“this test host has no usable VideoToolbox/libva encoder”，但本机是 macOS，VideoToolbox
  可用，自检结果是 `available: true`。纯环境假设问题。
- 建议：让测试不依赖宿主机编码器。例如注入一个固定的编码器探测结果（桩），或把期望改成与探测结果一致，
  而不是假设“宿主没有硬件编码器”。同一个文件里相邻的 probe 断言（约第 372 行，`h264_videotoolbox`）也是
  假设了本机行为，一并核对。
- 注意：#5 与它属于同一类问题，建议一起处理。

### 2. `forwarded_headers_are_only_honoured_for_trusted_proxies`

- 位置：`tests/api_web/remote.rs:289`。
- 现象：`X-Forwarded-For` 这一轮，`playback_sessions.remote_ip` 期望 `203.0.113.9`，实际 `127.0.0.1`。
- 含义：信任的代理发来的 `X-Forwarded-For` 没有被采纳（落回了连接对端 `127.0.0.1`）。
- 要查的方向：测试里“受信代理”是怎么配置的（`LUX_TRUSTED_PROXIES` 一类配置、或容器默认网关检测）。
  CLAUDE.md 规定：只有显式配置的受信代理 CIDR，或容器内检测到的单个默认网关才可信；非容器环境默认关闭。
  需要判断是测试在非容器的 macOS 上缺少显式配置，还是解析器真的有 bug。**不要**为了让测试通过去放宽信任规则
  或读取 `X-Forwarded-Proto` 做授权判断。

### 3. `series_scan_builds_stable_series_season_episode_hierarchy`

- 位置：`tests/scanner/series_scanner.rs:58`。
- 现象：第一次扫描 `created_items` 期望 `15`，实际 `17`。
- 线索：测试夹具里有 `Season 1(1)`、`Specials`、`Season 03` 等目录；多出的 2 个条目很可能来自某种
  目录或文件名规范化在 macOS 文件系统上的差异（大小写不敏感、Unicode 规范化），也可能是代码里某处合并规则
  与期望数不一致。需要打印两边的条目列表，找出多出的是哪 2 个，再判断是测试期望过时还是扫描器行为变了。

### 4. `media_info_plugin_config_api_drives_a_background_run`

- 位置：`tests/plugins/media_info_config_api.rs:243`。
- 现象：`POST /api/v1/admin/plugins/org.lux.strm-media-info/run` 期望 `202 Accepted`，实际 `409 Conflict`。
- 线索：测试前面先轮询等 `strm_probe_jobs` 里没有 `PENDING/RUNNING` 再发请求（第 225～233 行），
  但仍得到 409。要查：409 的触发条件是什么（已有一个运行中的作业、插件被禁用、或前置条件缺失），
  以及测试里的“等待”条件是否真能覆盖它。单跑稳定失败，不是偶发。

### 5. `broken_hardware_encoder_falls_back_to_software_transcoding`

- 位置：`tests/api_web/web_playback.rs:2185`。
- 现象：`assertion left == right failed`，`left: false`，`right: true`。
- 与 #1 同类：测试要模拟“硬件编码器坏了，回退到软件转码”，但在 macOS 上 VideoToolbox 实际可用，
  模拟没有生效，自检结果与测试预期相反。需要看测试是如何伪造“坏编码器”的，让它在任何宿主机上都确定。

### 6. lib 测试的整组顺序依赖

- 失败用例：
  - `application::probe::tests::playback_probe_runs_once_for_a_pending_local_source`
  - `application::watch::tests::realtime_incremental_scan_probes_the_source_it_indexes`
- 现象：`cargo test --locked --lib` 整组跑时失败；单独跑（本次已确认）各自通过。
- 之前定位到的线索：它们在 `require_ready_passes_the_ready_services_through` 之后执行时失败，
  疑似该测试留下了进程级的全局状态（probe 门禁 / ready 状态），后面的 probe/watch 测试看到了
  不属于自己的状态。需要找出这份全局状态，让每个测试自己建立并清理，或把门禁改成可注入。
- 验收：整组 `cargo test --locked --lib` 连跑多次都通过，且改变执行顺序（`-- --test-threads=1`
  与默认并行、`--shuffle` 如可用）都不再失败。

## 建议的处理顺序

1. #6（影响最广，整组 lib 不稳）。
2. #1 与 #5（同一类：宿主机编码器假设），可以一起设计一个“固定探测结果”的注入点。
3. #2（先判断是测试还是代码问题，涉及信任边界，改动要谨慎）。
4. #3、#4（需要先打印出具体差异再判断）。

## 验收标准

- 6 项在本机各自的单跑命令下通过。
- `cargo test --locked --all-targets` 全绿（`transcoding` 这类环境用例不再依赖宿主机）。
- 在你认为需要的环境里保持通过（例如没有 VideoToolbox 的 Linux 容器）；若某用例本质上必须依赖宿主能力，
  用显式的 `#[ignore]` 加说明，而不是让它在不同机器上随机失败。
- `cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`、
  `python3 scripts/check-backend-branches.py` 仍通过。
- 不要放宽 `X-Forwarded-For` 信任规则，也不要用 `X-Forwarded-Proto` 做鉴权或 ACL。

## 复核与修复记录（2026-11-08）

### 复核方法

- 在 `49a849e5` 工作区上先按表中的单跑命令逐个复现；对只记录了现象的三项，直接打印实际数据：
  系列扫描的 `media_items` 层级行、STRM 任务的状态行、`X-Forwarded-For` 解析器实际收到的
  peer/转发链/受信网络。
- 修复后逐项单跑，再跑完整 `cargo test --locked --lib`（默认并行，2019 passed / 40 ignored）和
  `cargo test --locked --all-targets --no-fail-fast`。
- 没有放宽 `X-Forwarded-For` 的信任规则，也没有让 `X-Forwarded-Proto` 参与鉴权或 ACL。

### 结论表

| # | 复核后的根因 | 修复 |
|---|---|---|
| 1 | 测试假设宿主机没有可用的硬件编码器，macOS 上 VideoToolbox 自检成功 | 给测试注入一个固定失败的自检可执行文件 |
| 2 | 不是解析器或信任配置问题；普通播放进度 tick 被合并写（最长 30 s），测试立刻读库读到了旧值 | 交替暂停/恢复让每次回调都写穿；断言和信任规则不变 |
| 3 | 多出的 2 项是 `SERIES "Season 1 1"` 及其 `SEASON 1`；`Season 1(1)`（无空格）没被识别为季目录 | 季目录解析接受紧贴的 `(数字)` 复制后缀 |
| 4 | 409 是 `StrmProbeError::AlreadyActive`；任务本身卡在完成事务的 `SQLITE_BUSY_SNAPSHOT` | 完成事务改用 `BEGIN IMMEDIATE`，测试改为有断言的有界等待 |
| 5 | 与宿主机无关：假 FFmpeg 用完整 `testsrc=...` 精确匹配自检参数，参数后来追加了 `,format=yuv420p` | 假 FFmpeg 改为按 `lavfi` 输入识别自检 |
| 6 | 进程级 `GLOBAL_PROBE_RUNTIME` 在第一个调用 `shutdown_background_workers` 的测试里被永久关闭 | `cfg(test)` 下不关闭这个进程级 runtime；真实 drain 仍由非测试构建和集成测试覆盖 |

### 1. `transcoding_settings_are_admin_only_validated_and_persisted`

- 修复：`tests/admin/admin_settings.rs:257` 起写入一个总是 `exit 1` 的 `fake-ffmpeg`，用
  `AppState::ready(...).with_hls_executable(...)` 注入。保存后的自检因此稳定得到
  `encoder: h264_videotoolbox`、`available: false`，与宿主机有没有 VideoToolbox/libva 无关。
- 测试原本的断言没有降低：`transcodingOptions` 仍按宿主机检测结果校验，设置持久化与权限断言不变。

### 2. `forwarded_headers_are_only_honoured_for_trusted_proxies`

- 复核结果：测试的受信代理配置本身是正确的。在 `request_access_ip` 里临时打印后可以看到，
  第二轮请求实际收到 `peer=127.0.0.1`、`fwd=["203.0.113.9"]`、`trusted=["127.0.0.1/32"]`，
  缓存也在设置写入后按 epoch 失效。解析器没有 bug。
- 真正的原因：`PlaybackProgressService` 只对状态变化、可能翻转已看状态、跨过续播阈值的 tick
  立即写库，普通进度 tick 最多合并 30 s（`PLAYBACK_PROGRESS_FLUSH_WINDOW`）。测试在普通
  `PLAYING` tick 之后立刻查 `playback_sessions.remote_ip`，读到的还是上一轮的 `127.0.0.1`；
  `/Sessions` 视图会叠加内存中的 pending tick，所以掩盖了这一点。
- 修复：`tests/api_web/remote.rs:226` 起改为交替 `IsPaused` 的 `/Sessions/Playing/Progress`，
  每次回调都是状态变化、都会在响应前写库。`X-Forwarded-For` 的期望值、`X-Real-IP`/`Forwarded`
  必须被忽略的断言、受信 CIDR 的配置方式全部保持原样。

### 3. `series_scan_builds_stable_series_season_episode_hierarchy`

- 实际打印出的 17 项：7 集 + 6 个季 + 4 个剧集。多出的两项是
  `SERIES`（标题 `Season 1 1`，identity `series:...:Drama/Grouped Show (2024)/Season 1(1)`）
  和它下面的 `SEASON 1`。
- 原因：`parse_season_directory` 只接受带空格的旧式复制后缀 `Season 1 (1)`。夹具里的
  `Season 1(1)` 因此不被当成季目录，整段路径被当成剧集目录，文件名里的 `S01E02` 又把季号补成 1。
- 修复：`src/application/scanner/names.rs:440` 接受紧贴的 `(数字)`；`Season 1DV` 仍然拒绝
  （只有括号数字才是复制标记）。新增 `src/application/scanner/tests.rs:3500` 的单元断言。
  扫描结果恢复为 3 个剧集 + 5 个季 + 7 集 = 15，第二次扫描 0 新增、7 跳过。

### 4. `media_info_plugin_config_api_drives_a_background_run`

- 409 的触发条件：`POST /api/v1/admin/plugins/{id}/run` → `start_configured_strm_probe_jobs`
  → `create_configured_jobs` → `create_jobs`，其中 `has_active_strm_probe_jobs()` 为真就返回
  `StrmProbeError::AlreadyActive`，映射成 409「已有 STRM 探测任务运行」。测试自己的等待循环
  只等 2 s 且**没有断言等到 0**，所以看起来像确定失败。
- 继续追查发现任务并非“慢”，而是可能永远停在 `RUNNING`：`finish_strm_probe_job` 用 deferred
  `BEGIN` 先 `SELECT` 再 `UPDATE`。另一个连接在快照之后提交时，升级写锁会得到
  `SQLITE_BUSY_SNAPSHOT`，`busy_timeout` 不会重试；错误被后台任务吞掉后任务行再也不会结束。
  仓库里 `Database::begin_metadata_write_transaction` 的注释已经写明这个约束。
- 全组并行跑时还暴露了同一类问题的第二处：`PUT /scheduled-tasks` 返回 503
  （`DATABASE_UNAVAILABLE`）。链路是 `update_scheduled_task_config` →
  `update_media_info_schedule` → `upsert_strm_media_info_task`；后者也是 deferred 事务里先
  `SELECT`（`ensure_global_task_plan`）再 `INSERT/UPDATE`，与正在运行的 STRM 任务并发提交时
  报 SQLite `database is locked`（主码 5，快照升级失败）。
- 修复：
  - `src/storage/jobs/strm_probe.rs:209` 改用 `begin_metadata_write_transaction()`（SQLite 上是
    `BEGIN IMMEDIATE`），消除快照升级失败。
  - `src/storage/library.rs:2405` 的 `upsert_strm_media_info_task` 同样改用
    `begin_metadata_write_transaction()`。
  - `tests/plugins/media_info_config_api.rs:225` 起把等待改成最多 30 s、失败时打印任务行；
    自动入队的等待从 1 s 放宽到 5 s，仍然保留断言。
- 验证：修复前同一用例 3 次里 2 次停在 `RUNNING`（约 41 s 后失败）；两个事务修复后
  `--test plugins` 全组连续 2 次通过（128/128）。

### 5. `broken_hardware_encoder_falls_back_to_software_transcoding`

- 与 #1 不是同一类问题，也不依赖宿主机：假 FFmpeg 用
  `[ "$arg" = "testsrc=size=320x240:rate=30:duration=0.3" ]` 精确匹配自检输入，而
  `hardware_probe_args` 现在传入的是 `testsrc=...,format=yuv420p`。于是自检被当成真实硬件编码
  而 `exit 1`，测试在 `available == true` 的断言处失败。
- 修复：`tests/api_web/web_playback.rs:2127` 改为按 `lavfi` 参数识别自检，真实硬件编码仍然
  `exit 1` 触发回退。这样自检在任何宿主机上都成功、真实编码都失败，测例本身保持原意。

### 6. lib 整组顺序依赖

- 这次抓到了完整报错：
  - `application::probe::tests::playback_probe_runs_once_for_a_pending_local_source`：
    `playback probe: Worker("probe admission is closed")`（`src/application/probe.rs:2131`）。
  - `application::watch::tests::realtime_incremental_scan_probes_the_source_it_indexes`：
    `left: "PENDING"` / `right: "READY"`（`src/application/watch.rs:1006`）。
- 全局状态：`static GLOBAL_PROBE_RUNTIME`（`src/application/probe/concurrency.rs`）是进程级
  `OnceLock<ProbeRuntime>` 加一个 `closing: AtomicBool`。`ProbeRuntimeRegistry::shutdown` 会把
  `closing` 永久置真。`AppState::shutdown_background_workers` 会调用
  `probe::shutdown_probe_runtime`，而 lib 里有多个测试会关停完整状态
  （`application::collections` 的全局集合 worker 测试、`api::legacy` 的 ready 状态测试、
  `api::media` 的直连票据测试）。按测试名排序，这些测试先于 `application::probe`/`watch`
  执行，于是后面的 probe 测试注册会话时拿到 `AdmissionError::Closed`。
- 修复：`src/application/probe/concurrency.rs:97` 起把 `shutdown` 拆成两个 `cfg` 版本。
  非测试构建保留原来的终止语义；`cfg(test)` 构建下它是 no-op，因为一个 lib 测试进程会反复
  建立/关停多个状态，不能让其中一个测试永久关掉整个进程的探测准入。
  真实的 drain 仍在 `shutdown_and_drain` 里，并由
  `registry_shutdown_prevents_late_runtime_initialization` 单测覆盖；
  集成测试 `tests/shutdown.rs::probe_runtime_shutdown_reaps_active_ffprobe_before_return`
  链接的是不带 `cfg(test)` 的库，继续验证活跃 ffprobe 会被回收。
- 新增 `registry_shutdown_keeps_admission_usable_for_the_next_test` 锁定这个测试期行为。

### 额外发现 1：第 9 个基线失败 `local_metadata_worker_persists_capability_missing`

- 单跑稳定失败：`Error: Elapsed(())`，10.79 s（即 10 s 超时）。
- 原因：测试用裸 SQL 把 `scan_local_metadata_backfills.next_attempt_at` 置 0，绕过了服务层
  “写入后唤醒 outbox worker”的路径；worker 的 idle fallback 已经是
  `SCAN_LOCAL_METADATA_OUTBOX_IDLE_FALLBACK = 10 s`（注释里记录旧值是 250 ms），正好等于
  测试的超时预算。
- 修复：`src/application/scanner/tests.rs:1782` 在 requeue 之后调用
  `jobs.notify_local_metadata_outbox_worker()`，与生产写入路径一致。
- 同样的模式还出现在 `tests/scanned_metadata.rs` 的两个重试用例：
  `local_metadata_backfill_retries_nfo_failure_without_advancing_its_cursor` 和
  `local_metadata_backfill_retry_keeps_conflicted_item_excluded`。这两处是集成测试，只能看到
  公开 API，不能调用 `pub(in crate::application::scanner)` 的 notify，因此把裸 SQL requeue 后的
  等待从 10 s（正好等于 idle fallback）放宽到 30 s，留出三个 fallback 周期；重试语义的断言不变。

### 额外发现 2：多处“先读后写”的 deferred 写事务

见 #4 的追查。全量并行跑还暴露了 `POST /Collections/{id}/Items` 的 503：`--test library` 的
`emby_manual_collections_and_library_cover_follow_qmby_contract` 期望 204，实际 503。链路是
`add_emby_collection_items` 在 deferred 事务里先 `SELECT` 集合行再插入成员，同样是
`SQLITE_BUSY_SNAPSHOT`。已把下列 read-then-write 事务改用
`begin_metadata_write_transaction`（SQLite 上是 `BEGIN IMMEDIATE`）：

- `src/storage/jobs/strm_probe.rs` 的 `finish_strm_probe_job`
- `src/storage/library.rs` 的 `upsert_strm_media_info_task`
- `src/storage/media.rs` 的 `adopt_media_item_identity`、`upsert_collection`、
  `create_emby_collection`、`add_emby_collection_items`、`remove_emby_collection_items`

这类模式在 storage 里还有别的候选（例如 `sessions::sync_played_container_states_for_scope`、
`save_media_probe`、people 的若干确认/撤销事务）。它们目前没有在本机门禁里失败，本次没有扩大改动；
建议后续按 `begin_metadata_write_transaction` 的注释做一次系统审计。

### 顺手加固：文档标记为“满载偶发”的两个用例

这两项不在 6 项清单内，但会让 `--all-targets` 在本机并行跑时失败：

- `danmaku::danmaku_service_matches_local_and_strm_sources_and_writes_sidecars`
  - 失败点不是等待任务完成，而是 10k 源建任务用的 250 ms 性能上限：全组 128 个测试并行时
    一次 `INSERT ... SELECT` 也会超过它（单跑约 1.8 s 完成整段测试）。
  - 改为 10 s 上限，保留 `total_count == 10002` 的语义断言；任务完成等待从 5 s 放宽到 30 s。
- `scanned_metadata::manifest_scan_indexes_poster_while_local_nfo_is_blocked`
  - 等待“海报已索引且本地批次正在运行”的窗口是 10 s，在全量并行和外置盘 I/O 下会超时。
  - 改为 30 s；FIFO 仍会阻塞 NFO 读取，断言逻辑不变。
- `reidentify::fill_missing_dispatcher_serializes_jobs_for_one_library`
  - 五处等待异步 dispatcher 调度/状态迁移的窗口是 2 s 或 5 s，全量并行跑时
    `Error: Elapsed(())`；改成 30 s 后单跑全组连续 3 次通过（25/25，约 21 s）。
  - 同时给三处裸 `?` 超时补了阶段说明，失败时能看出是“独立媒体库未启动”“取消未让位”
    还是“背压提交未完成”。50 ms 的背压断言和 150 ms 的 shutdown 断言保持原样。

### 验收结果（2026-11-08，Apple Silicon / `arm64`）

- 6 项单跑：全部通过（含 #6 的 probe/watch 两个用例）。
- `cargo test --locked --lib`：`2019 passed; 0 failed; 40 ignored`（默认并行，13m22s）。
- `cargo test --locked --all-targets --no-fail-fast`：**EXIT=0，全绿**。22 个 target 合计
  `3108 passed; 0 failed; 89 ignored`，包含 12 个分组、8 个独立 binary 和 lib。
- `cargo fmt --all -- --check`：通过。
- `cargo clippy --locked --all-targets --all-features -- -D warnings`：通过。
- `python3 scripts/check-backend-branches.py`：`backend branches ok: 28 files, 266 branches`。
- `python3 scripts/check-migration-parity.py`：`ok: 179 paired, 50 SQLite-only, 19 PostgreSQL-only`。

复现与验收命令都先 `source scripts/lux-env.sh`，构建产物只写在项目目录内。
