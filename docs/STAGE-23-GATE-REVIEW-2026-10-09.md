# 阶段 23 阶段门证据复核（2026-10-09，当前候选树 d5b5e59e + 工作树修复）

## 结论与复核范围

**阶段 23 仍开放，不能关闭。** 当前隔离树 HEAD 为 `d5b5e59e3825e1725f066e72dacd5e417dff0fae`，在该提交上另有补缺空搜索回归的未提交代码修改；release A/B 候选测试二进制 SHA-256 为 `0a23c327c5a36b6829aa9c727cdf30579af933088d7ed0cddad084359dd5198e`。候选性能代码与 `21c8dfc7` 相同，补缺分类修改不在这些扫描基准的调用路径中。LUX-469 故障修复及 #10/#13/#16/#17 相关工作已纳入此候选树。

最终性能矩阵仍未完成。现有最终候选样本为 1/24 个有效配对：LUX-270、SQLite、1k、第一轮，baseline `6424ab12` 对候选 `21c8dfc7`。性能记录尚未写入 `docs/PERFORMANCE.md`；这一个配对只作早期回退信号，不能替代当前候选树的三轮完整矩阵。此前针对 `e6a7ef8a` 的三轮数据仍只作为历史风险证据。

阶段末需要完成指定检查、更新兼容性或性能记录并取得项目所有者确认。以下审查不替代该确认。

## 六项验收证据矩阵

| # | 验收项 | 当前证据 | 判定与剩余缺口 |
|---|---|---|---|
| 1 | 1k/10k fixture 证明首批索引和本地 poster 在扫描完成前可见，worker 与后续索引并行 | 候选含 `tests/scanned_metadata.rs::manifest_scan_indexes_poster_while_local_nfo_is_blocked`、`blocked_first_local_poster_does_not_block_manifest_scan` 和 `workflow3_running_discovery_indexes_poster_added_to_unvisited_directory`。最后一项在最终整理版精确测试通过，并在串行目标中通过。LUX-462 有单轮 SQLite/PostgreSQL 观测；旧 LUX-306 有 1k/10k poster-worker A/B。 | **部分完成。** 新回归证明 SQLite fixture 中扫描与本地 worker 并行；仍缺当前候选同 fixture 1k/10k、SQLite/PostgreSQL 的 LUX-270 与 poster-worker A/B。现有记录未单独证明 Web 在该阶段实际呈现首批条目。依据 `docs/LUX-DEVELOPMENT.md` 阶段 23 第 1 项。 |
| 2 | 首项图片、后段图片、慢 NFO、权限、根不可用、取消/重试、全量/增量竞态及扫描期间新 poster 的故障覆盖 | 候选文档映射到 `manifest_scan_indexes_poster_while_local_nfo_is_blocked`、`blocked_first_local_poster_does_not_block_manifest_scan`、`failed_local_poster_insert_does_not_mark_image_stage_complete`、两项 `unreadable_*_poster_*`、`unavailable_media_root_keeps_local_image_batch_retryable`、`cancelling_scan_during_running_local_image_claim_cancels_before_completeness`、Manifest/incremental CAS 用例及新增 poster 用例。最终三个 target 串行结果为 `scanned_metadata` 21/21、`scanned_series_metadata` 3/3、`scanning_jobs` 81/81；新增 poster 精确回归 1/1。 | **故障覆盖已完成。** 结果是本机 SQLite 自动化 fixture，不证明 PostgreSQL、FNOS 或 NAS 运行行为。此前报告中“LUX-469 未进入候选、target/build/Clippy 尚无结果”的表述已过时。证据见 `docs/LUX-DEVELOPMENT.md` LUX-469 进度记录。 |
| 3 | 缺失分类、自动补缺策略、队列去重、无候选/失败冷却、执行前复查和保护本地/锁定字段 | 除原有策略、去重、锁定字段与 LUX-402/LUX-407 回归外，当前树新增 `fill_missing_empty_provider_search_is_low_confidence_not_unavailable` 和 `fill_missing_episode_without_series_candidates_is_not_unavailable`。每项都确认 job 完成而非 `DEFERRED`、item `LOW_CONFIDENCE`、`candidate_count=0`、无候选行且 title/provider IDs 不变；真实 provider 请求失败仍由 `scraper_unavailable_metadata_job_is_deferred` 覆盖。 | **空候选分类缺口已补齐。** `cargo test --locked --test reidentify -- --test-threads=1` 在本机 ARM64/SQLite 通过 19/19。既有 LUX-402 回归继续覆盖 provider 失败退避/重试；阶段最终表仍需保留策略—测试—断言映射。依据 `tests/reidentify.rs`、`tests/metadata_selection.rs`、`src/application/candidates.rs`、`src/application/scanner.rs` 及 LUX-402/LUX-407 记录。 |
| 4 | SQLite/PostgreSQL 同 fixture 性能 A/B：索引、首批可见、poster、本地/在线队列、前台 p95、事务/队列规模、内存；超过 5% 回退后调整并复测 | 已有 1 个有效配对：LUX-270 SQLite 1k，baseline `6424ab12` 对 candidate `21c8dfc7`，第一轮；index `57→68 ms`、复合 unchanged-rescan 窗口 `1,119→1,668 ms`、foreground p95 `540→813 ms`、catalog p95 `543→807 ms`、DML `37→34`、peak RSS `161,988,608→510,263,296 bytes`。 | **性能门未通过，且当前候选树完整矩阵尚待复测。** 该单轮 p95/RSS 是回退风险信号，不作三轮结论；unchanged-rescan 含并发 HTTP 请求，不是纯 scan wall time。还缺 LUX-270 与 LUX-304 的 1k/10k × SQLite/PostgreSQL × 三轮完整配对；旧 baseline 的 LUX-304 harness 需按其实际“图片在 metadata stage 内同步完成、没有 detached image batch”的路径测量，不能用 0153 列或候选 batch marker 伪造旧行为。不得降低 5% 门槛。 |
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

1. 对当前候选树完成 LUX-270 的 1k/10k × SQLite/PostgreSQL 与 LUX-304 poster-worker 的 1k/10k × SQLite/PostgreSQL A/B；每种配置 baseline/candidate 各三轮，交错轮换顺序。记录首项/首 poster、索引、本地队列、在线 job 是否创建、前台 p95、事务/队列规模、内存及独立阶段计时。在线队列未由该 fixture 触发时明确记为未测。对稳定超过 5% 的回退先分段定位、调整并按阶段门重测。
2. 在最终候选上重跑全 Rust completion gate；完整 all-targets 必须有稳定、可解释且通过的结果，不能用单个隔离重跑代替。保留已通过的 Web 冻结安装、完整测试和 build 证据，并记录先前波动及隔离复跑。
3. 更新 `docs/PERFORMANCE.md` 和 `docs/COMPATIBILITY.md`，写明候选二进制/源码树、采样与测试证据及 ARM64 边界；不把本机结果外推为 FNOS/NAS 性能或已部署运行证明。
4. 所有技术项通过后停在阶段门，等待项目所有者明确确认。确认前阶段 23 保持开放。
