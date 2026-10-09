# 阶段 23 阶段门证据复核（2026-10-09）

## 结论与复核范围

**阶段门保持开放，不能关闭。** 本次以 gate worktree 的候选 `e6a7ef8a14122a7affda50a78042e7c6c4e89b2f` 为代码基线，只核对阶段 23 总体验收、相关测试和当前 A/B 证据；没有运行 Cargo，也没有修改业务代码。故障覆盖候选 `9ec104608ac229f57f2951adc2bf09724645283d` 目前在独立 worktree，未包含在本次候选 SHA 中。

`docs/LUX-DEVELOPMENT.md` 的阶段门规定阶段末必须运行阶段检查、更新兼容性或性能记录，并由项目所有者确认。以下六项仍未满足全部关闭条件；本复核不勾选原清单，也不代替项目所有者确认。

## 六项验收证据矩阵

| # | 验收项 | 已有证据 | 缺口与关闭判定 |
|---|---|---|---|
| 1 | 1k/10k fixture 中，首批索引与本地海报在扫描结束前可查，且本地 worker 与后续索引并行 | 当前候选有 `tests/scanned_metadata.rs::manifest_scan_indexes_poster_while_local_nfo_is_blocked`，覆盖 NFO 阻塞期间 poster 可见。较早的 LUX-462 单轮记录报告 10k 首项/首张 poster 均在 88 ms 可见、scan job 在 1,470 ms 完成、本地图片队列在 4,380 ms 排空（`docs/PERFORMANCE.md`，基准候选为 `e487076b`）。 | 单轮记录不是当前候选 A/B，也没有证明真实 Web UI 已显示首批条目。10k 同 fixture 的当前候选对照、poster-worker 矩阵尚未完成。**部分覆盖，不能关闭。** |
| 2 | 首项图片/后段海报、慢 NFO、权限错误、不可用根、取消重试、全量/增量竞态及扫描期间本地补图都有自动化覆盖 | 当前候选已有慢 NFO、图片写事务失败、增量扫描和定向本地补图测试；`tests/scanning_jobs.rs` 有 Manifest 与 incremental 文件变更竞争回归。独立提交 `9ec10460` 增加首次 poster 阻塞、RUNNING local image claim 中取消、movie/series poster `PermissionDenied` 重试及 media root 不可用用例；故障 agent 报告权限/取消用例先红后绿，首次 poster 阻塞与不可用根用例定向各通过 1/1。 | `9ec10460` 未集成进 `e6a7ef8a`。故障 agent 正在运行 `scanned_metadata`、`scanned_series_metadata` 与 `scanning_jobs` 三个完整 target；截至本记录更新时，完整 target、build、Clippy 尚无结果（fmt 已报告通过）。全量/增量已有回归主要验证索引状态，不足以替代全部 local image worker 竞态验收。**部分覆盖，待集成及完整验证。** |
| 3 | 缺失分类、自动补缺策略、任务去重、provider 无候选/失败冷却、执行前复查及保护本地/锁定数据均有回归 | 已有较广的底层测试：`tests/reidentify.rs` 覆盖完整条目跳过刮削、同库 dispatcher 合并与活动任务约束；`tests/metadata_selection.rs` 覆盖缺字段选择、锁定 NFO 保留、图片 backoff 和显式空结果；`tests/scanned_metadata.rs` 覆盖 completeness 与 NFO 冲突重试。 | 现有记录未给出一份把清单中的每个 provider 状态、冷却期限、执行前再次检查和 workflow 3 自动调度接线逐一映射到测试的证据表；不能仅凭底层用例名称推定整条新流程完整。**部分覆盖，逐项映射/缺项回归仍需完成。** |
| 4 | SQLite/PostgreSQL 同 fixture A/B 分别报告索引、首批/首 poster、local/online queue、前台 p95、事务/队列规模和内存；超过 5% 回退先调度/并发并复测 | 2026-10-09 当前 1k LUX-270 A/B 为 baseline `6424ab122a556e2214f9948580b8c7bf42407aa3` 对 candidate `e6a7ef8a14122a7affda50a78042e7c6c4e89b2f`，SQLite/PostgreSQL 各三轮。索引中位数 SQLite `64→54 ms`、PostgreSQL `146→149 ms`。但 `foregroundP95` 为 SQLite `547→791 ms`（+44.6%）、PostgreSQL `557→792 ms`（+42.2%）；热目录页 p95 分别 `529→772 ms` 与 `543→770 ms`，均超过 5% 门槛。 | 这是当前候选**性能门未通过**的证据。10k LUX-270 与 1k/10k poster-worker A/B 尚未运行；online queue 和所需事务/队列规模、内存字段也未形成完整双后端矩阵。回退根因仍未分离；SQL 窗口含后台语句、elapsed 不含 pool acquire，当前还缺每请求 pool acquire、storage await、映射/JSON 与 CPU 拆分。**未通过，必须先定位并修复/重测，不能关闭。** |
| 5 | 扫描索引耗时与本地/在线处理耗时分开报告，不把剩余后台工作混入索引耗时 | 既有 LUX-304 记录分别列 scan job 与 local poster queue；LUX-462 增加 image-pending 与严格 drain 窗口。A/B harness 分别输出 index、目录请求和热页字段。 | 当前 `unchangedRescanMs` 是从 rescan 开始到结束的复合窗口，其中包含前后两组各 50 个并发 HTTP 请求，不能解释为纯扫描耗时。在线 queue 缺少当前双后端实测；最终矩阵尚未完成。**部分覆盖，当前 A/B 仍需按阶段拆分并补齐。** |
| 6 | 相关 Rust/Web 全量质量门、兼容性/性能记录和架构记录完成，并由项目所有者确认 | `docs/LUX-DEVELOPMENT.md` 的 LUX-463 记录在 `e6a7ef8a` 说明 all-target Rust 测试、build、fmt、Clippy 和差异检查通过（library 808 passed、13 ignored）；性能资料记录了 ARM64 与 PostgreSQL 16.15 边界。 | LUX-469 故障覆盖尚未进入该 SHA，完整 targets 与质量检查待补；本阶段没有当前最终候选的 Web test/build 记录。`docs/COMPATIBILITY.md` 的 LUX-288 段仍写明 workflow 3/增量事件/在线补缺未由该记录证明，阶段 23 兼容性结论待追加。**项目所有者确认尚未取得；此项不得由 agent 自动关闭。** |

## 1k A/B 回退样本说明

三轮中位数如下，p95 字段为 A/B harness 中的 50 并发目录请求观测：

| 后端 | 指标 | Baseline → candidate | 变化 |
|---|---|---:|---:|
| SQLite | index | 64 → 54 ms | -15.6% |
| SQLite | foreground p95 | 547 → 791 ms | +44.6% |
| SQLite | catalog p95 | 566 → 771 ms | +36.2% |
| SQLite | warm catalog p95 | 529 → 772 ms | +45.9% |
| SQLite | no-change composite window | 1,146 → 1,611 ms | +40.6% |
| PostgreSQL | index | 146 → 149 ms | +2.1% |
| PostgreSQL | foreground p95 | 557 → 792 ms | +42.2% |
| PostgreSQL | catalog p95 | 541 → 765 ms | +41.4% |
| PostgreSQL | warm catalog p95 | 543 → 770 ms | +41.8% |
| PostgreSQL | no-change composite window | 1,142 → 1,575 ms | +37.9% |

`no-change composite window` 包含两组并发 HTTP burst，不可用作纯 scan wall-time。现有样本未观察到能稳定解释回退的 pool 饱和或 PostgreSQL lock 等待；PostgreSQL 热页 SQL elapsed 中位数从约 383.5 ms 增至 657.6 ms/50 请求，SQLite 从约 23.54 ms 增至 25.74 ms/50 请求。但 SQL phase window 可能混入后台查询，elapsed 不包含获取连接的等待，因此这些数字只能说明需继续拆分，不能确定代码根因或据此宣称已完成修复。

## 仍需完成的关闭步骤

1. 把 LUX-469 `9ec10460` 的测试修复与实现集成到最终候选；完成两项扫描 integration target、build、fmt、Clippy，并复核已有/新增测试覆盖矩阵。
2. 完成 10k LUX-270 及 1k/10k poster-worker SQLite/PostgreSQL A/B；将索引、local queue、online queue、前台请求、资源/事务规模分开记录。对目前超过 5% 的前台 p95 回退，先取得可归因的分段证据，再决定候选是否需调整并复测。
3. 在兼容性事实来源中补充 workflow 3 的真实合同与自动化证据；更新性能记录并明确本机 ARM64 结论不外推为 FNOS/NAS/x86_64 性能。
4. 对集成后的最终 SHA 执行阶段要求的 Rust/Web 质量门。全部技术验收通过后，**停在阶段门等待项目所有者确认**；收到明确确认之前，阶段 23 仍保持开放。
