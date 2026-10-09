# 阶段 23 阶段门证据复核（2026-10-09，更新至候选 21c8dfc7）

## 结论与复核范围

**阶段 23 仍开放，不能关闭。** 本次按已推送候选 `21c8dfc7781e0ce4a0e698fe59e6abed2dba7265` 复核；该 SHA 在复核时是 `origin/test` 与 `codex/lux-audit-optimization-round` 的 HEAD。只更新本阶段门报告，没有运行 Cargo 或性能采样。候选已包含 LUX-469 故障修复及 #10/#13/#16/#17 相关提交；先前报告将 `e6a7ef8a` 当作当前候选并声称 LUX-469 尚未集成，已过时。

最终性能矩阵尚无结果。A/B agent 已收到 `21c8dfc7` 并开始准备最终矩阵；本次复核收到的性能文件仍只有针对中间候选 `e6a7ef8a` 的 1k A/B 数据。候选 SHA、对应测试状态和历史性能样本必须分别陈述，不能相互替代。

阶段末需要完成指定检查、更新兼容性或性能记录并取得项目所有者确认。以下审查不替代该确认。

## 六项验收证据矩阵

| # | 验收项 | 当前证据 | 判定与剩余缺口 |
|---|---|---|---|
| 1 | 1k/10k fixture 证明首批索引和本地 poster 在扫描完成前可见，worker 与后续索引并行 | 候选含 `tests/scanned_metadata.rs::manifest_scan_indexes_poster_while_local_nfo_is_blocked`、`blocked_first_local_poster_does_not_block_manifest_scan` 和 `workflow3_running_discovery_indexes_poster_added_to_unvisited_directory`。最后一项在最终整理版精确测试通过，并在串行目标中通过。LUX-462 有单轮 SQLite/PostgreSQL 观测；旧 LUX-306 有 1k/10k poster-worker A/B。 | **部分完成。** 新回归证明 SQLite fixture 中扫描与本地 worker 并行；仍缺当前候选同 fixture 1k/10k、SQLite/PostgreSQL 的 LUX-270 与 poster-worker A/B。现有记录未单独证明 Web 在该阶段实际呈现首批条目。依据 `docs/LUX-DEVELOPMENT.md` 阶段 23 第 1 项。 |
| 2 | 首项图片、后段图片、慢 NFO、权限、根不可用、取消/重试、全量/增量竞态及扫描期间新 poster 的故障覆盖 | 候选文档映射到 `manifest_scan_indexes_poster_while_local_nfo_is_blocked`、`blocked_first_local_poster_does_not_block_manifest_scan`、`failed_local_poster_insert_does_not_mark_image_stage_complete`、两项 `unreadable_*_poster_*`、`unavailable_media_root_keeps_local_image_batch_retryable`、`cancelling_scan_during_running_local_image_claim_cancels_before_completeness`、Manifest/incremental CAS 用例及新增 poster 用例。最终三个 target 串行结果为 `scanned_metadata` 21/21、`scanned_series_metadata` 3/3、`scanning_jobs` 81/81；新增 poster 精确回归 1/1。 | **故障覆盖已完成。** 结果是本机 SQLite 自动化 fixture，不证明 PostgreSQL、FNOS 或 NAS 运行行为。此前报告中“LUX-469 未进入候选、target/build/Clippy 尚无结果”的表述已过时。证据见 `docs/LUX-DEVELOPMENT.md` LUX-469 进度记录。 |
| 3 | 缺失分类、自动补缺策略、队列去重、无候选/失败冷却、执行前复查和保护本地/锁定字段 | 候选包含 `fill_missing_request_plan_only_keeps_missing_capabilities`、`fill_missing_skips_complete_local_items_before_resolving_scrapers`、`scraper_preflight_failure_enters_bounded_fill_missing_retry`、`incremental_scan_only_queues_metadata_when_library_switch_is_enabled`、`idle_fill_missing_dispatcher_coalesces_requests_before_claim`、`full_refresh_preserves_locked_nfo_fields_and_replaces_existing_images` 等回归。LUX-402 记录同一 fingerprint/capability 退避、到期重试及失败次数继承；LUX-407 区分预检错误、明确无 scraper 和无可请求 capability。 | **大体有覆盖，但仍需把证据逐条对齐验收。** 代码中有返回空候选的 stub 和 dispatcher 用例；现有记录未清楚指出哪条断言专门证明“provider 无候选”后的结果及重试/冷却边界。建议最终覆盖表明确对应测试/断言；若没有直接断言，补定向回归。依据 `tests/reidentify.rs`、`tests/metadata_selection.rs`、`src/application/candidates.rs`、`src/application/scanner.rs` 及 `docs/LUX-DEVELOPMENT.md` 的 LUX-402/LUX-407。 |
| 4 | SQLite/PostgreSQL 同 fixture 性能 A/B：索引、首批可见、poster、本地/在线队列、前台 p95、事务/队列规模、内存；超过 5% 回退后调整并复测 | 当前性能记录中的阶段 23 1k LUX-270 A/B 是 baseline `6424ab12` 对 candidate `e6a7ef8a`，各后端三轮。其前台 p95 SQLite `547→791 ms`（+44.6%）、PostgreSQL `557→792 ms`（+42.2%），热目录 p95 同样明显回退；索引中位数 SQLite `64→54 ms`、PostgreSQL `146→149 ms`。 | **未通过，且不是候选 21c8dfc7 的最终结论。** 旧数据保留为历史性能风险；候选 21c8dfc7 的完整矩阵尚未运行/记录，尚无证据证明回退已消失。还缺 LUX-270 的 1k/10k、LUX-304 poster-worker 的 1k/10k 双后端多轮对照及在线队列、事务/队列规模和内存数据。不得降低 5% 门槛。依据 `docs/PERFORMANCE.md` 的阶段 23 / LUX-270 初步 A/B。 |
| 5 | 分开报告索引、本地处理和在线处理耗时 | LUX-462 harness 已分别记录 index、scan job、image-pending/drain 及请求窗口；LUX-304 历史结果也分别呈现 scan job 与 poster queue。 | **部分完成。** 阶段 23 旧 1k `unchangedRescanMs` 包含 rescan 与两组并发 HTTP 请求，是复合窗口而非纯扫描耗时；完整最终候选 A/B 和 online queue 分段数据仍待采集。最终报告需分开索引、本地队列、在线队列与前台负载。依据 `docs/PERFORMANCE.md` 中 1k A/B 的窗口定义及 LUX-462 记录。 |
| 6 | 最终 Rust/Web 质量门、兼容性/性能记录、架构记录及项目所有者确认 | 当前 SHA 的串行 Rust all-targets 结果为 819 passed、1 failed、13 ignored；唯一失败 `application::danmaku::tests::configured_jobs_reuse_plugin_settings_across_libraries`，断言期望 19、实得 20；该测试在同一 SHA 隔离重跑 1/1 通过。LUX-469 三个 target 通过。Web 冻结安装通过；完整 Web 重跑为 132 个 Node 测试及 Vitest 76 个文件/561 项通过；首次并发运行出现 5 个首页用例波动，隔离文件 17/17 通过；Web build 通过。 | **阶段门未通过。** Rust 全量测试的原始阶段结果仍有 1 个失败，即使隔离重跑通过也不能将 all-targets 标为全绿；需稳定复跑全量门并调查波动来源。Web 门通过。LUX-288 `docs/COMPATIBILITY.md` 第 243 行仍写 workflow 3/增量事件/在线补缺运行合同和 A/B 未证明，应在阶段证据完成后更新。项目所有者尚未确认；这是技术验收全过后的最后停点，agent 不得代为关闭。 |

## 旧 1k A/B：保留为风险证据，不代表当前候选

以下是中间候选 `e6a7ef8a14122a7affda50a78042e7c6c4e89b2f` 的三轮中位数，单位 ms。它早于当前 `21c8dfc7`，所以既不能证明当前候选的回退，也不能证明回退已修复。

| 后端 | 指标 | Baseline → `e6a7ef8a` | 变化 |
|---|---|---:|---:|
| SQLite | index | 64 → 54 | −15.6% |
| SQLite | foreground p95 | 547 → 791 | +44.6% |
| SQLite | catalog p95 | 566 → 771 | +36.2% |
| SQLite | warm catalog p95 | 529 → 772 | +45.9% |
| PostgreSQL | index | 146 → 149 | +2.1% |
| PostgreSQL | foreground p95 | 557 → 792 | +42.2% |
| PostgreSQL | catalog p95 | 541 → 765 | +41.4% |
| PostgreSQL | warm catalog p95 | 543 → 770 | +41.8% |

旧 `unchangedRescanMs` 包含并发 HTTP 请求测量，不作为纯 scan wall time。该基准的 SQL elapsed 采样可能混入后台 SQL，且不含连接池 acquire 时间，不能单独归因代码根因。A/B 证据限定为本机 ARM64 和本地 PostgreSQL 16.15；不能外推 FNOS/NAS/x86_64。

## 最短剩余清单

1. 对候选 `21c8dfc7781e0ce4a0e698fe59e6abed2dba7265` 完成 LUX-270 的 1k/10k × SQLite/PostgreSQL 与 LUX-304 poster-worker 的 1k/10k × SQLite/PostgreSQL A/B；每种配置 baseline/candidate 各三轮，明确记录首项/首 poster、索引、本地与在线队列、前台 p95、资源/事务/队列规模、内存及独立阶段计时。对超过 5% 的回退，先分段定位、调整和复测。
2. 完成第 3 项的逐项“行为—测试—断言—运行结果”映射；补齐 provider 无候选的直接断言（若现有测试没有覆盖）。
3. 在最终候选上重跑全 Rust completion gate；完整 all-targets 必须有可解释且通过的结果，不能用单个隔离重跑代替。保留已经通过的 Web 冻结安装、完整测试和 build 证据；将初次波动和隔离复跑结果写入记录。
4. 更新 `docs/PERFORMANCE.md` 和 `docs/COMPATIBILITY.md`，写明精确候选 SHA、采样与测试证据和 ARM64 边界；不要把本机结果外推为 FNOS/NAS 性能或已部署运行证明。
5. 所有技术项通过后停在阶段门，等待项目所有者明确确认。确认前阶段 23 保持开放。
