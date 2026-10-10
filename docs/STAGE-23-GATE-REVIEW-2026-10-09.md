# 阶段 23 阶段门证据复核（2026-10-09，更新 2026-10-10）

## 结论与 revision

**阶段 23 未完成，性能门不能判通过。** 本次冻结测量/常规 Rust 质量门对应 `32c158ecbe1196db7a53a39e7f6ba8149c93095b`，基线为 `6424ab122a556e2214f9948580b8c7bf42407aa3` 加相同测量补丁；旧 `e6a7ef8a` 的不公平 1k 样本不作为回退或达标证据。完整的计时合同、数据表、失败与归档位置见 [PERFORMANCE.md](PERFORMANCE.md) 的“阶段 23 公平计时复核与未关闭门”。

另一会话已将冻结提交链集成并推送到 `test`；本轮观察到 `test/origin/test=0980dd36`。该版本还有扫描默认并发 4、metadata worker 配置和其他后续修改。本记录分开列出旧冻结运行与新 Web 测试，不把旧性能或 Rust 门覆盖到新版本。本轮没有推送记录增量或部署。

最终远端只读刷新又观察到 `origin/test=b4635ee3946d497bb1d122ea6597ff2df0a4bc9f`（PR #43 的 PostgreSQL movie merge 修复）；`origin/main=4ee1ea5f`。记录工作树仍基于 `0980dd36`，测量仍为 `32c158ec`；后续源码也没有在本轮 Rust/性能证据的覆盖范围内。

## 六项验收证据矩阵

| # | 验收项 | 已运行证据 | 判定与剩余缺口 |
|---|---|---|---|
| 1 | 首批索引、local poster 早于 scan 完成；worker 与后续索引并行，Web 刷新 | `32c158ec` 的 1k/10k 双后端 poster 运行均有先后断言；LUX-469 FIFO/慢 NFO/新 poster 回归；`18b88888` 的组件回归验证 mock 空首页→home 通知→条目和 poster URL；未建模 scan lifecycle 或加载图片。 | **部分完成。** SQLite poster 各缺一轮；DOM 是静态组件 mock，不证明扫描 lifecycle、图片实际加载或浏览器/真实扫描端到端行为。 |
| 2 | 首项/后段图片、慢 NFO、权限、不可用根、取消/重试、全量/增量竞态、扫描期间补图 | 最终 `scanned_metadata` 22/22、`scanned_series_metadata` 3/3、`scanning_jobs` 81/81；LUX-469 测试映射保留于规格。 | **故障覆盖通过于冻结 SHA。** 这是本机 SQLite 自动化 fixture；后续修改需对应重验。 |
| 3 | 缺失分类、策略、去重、无候选/失败冷却、执行前复查、保护本地/锁定数据 | `local_metadata_worker_dispatches_fill_missing_without_blocking_scan` 释放空 provider 后专门检查 item `COMPLETED/LOW_CONFIDENCE`、retry=0、retry_after=NULL；1k 双后端 online fixture 全部 1000 项也满足相同断言；metadata_selection 31/31、reidentify 集成 17/17、reidentify 模块 28/28。 | **小 fixture 回归通过，规模边界失败。** 10k 自动调度受容量 32 channel 背压阻塞 local worker，需先解决 LUX-424/LUX-303 冲突。 |
| 4 | 双后端同 fixture、至少三轮；索引/首项/poster/local/online/p95/事务与队列/RSS；回退 ≤5% | 原始 48 行，3 对发现 Cargo 干扰并剔除；仅 LUX-270/10k SQLite 成对补跑成功，现有 44 行诊断数据。逐请求与窗口完整保存。 | **未通过。** SQLite poster 两组各只有两轮；基线默认并发 16、候选 2，未跑相同并发对照；最新 test 默认 4。索引及部分 PG p95 超过 5%，尚未归因；PG poster queue 后 p95 约 +19%/+22%。不能降低门槛或用默认配置差异归因代码。 |
| 5 | 索引、本地、在线分开计时 | 独立 scan task 与 poster observer 修正；1k online 真实触发 10 jobs/1000 items，分别报告 gate hold 与释放后 drain（SQLite 668ms、PG 2494ms）。 | **部分完成。** 10k SQLite provider 未放行时 local queue 超时；10k PG 未运行。失败/未触发保持 unavailable，不能记零。 |
| 6 | 最终 Rust/Web 门、兼容/性能/架构记录、ARM 记录及所有者确认 | `32c158ec` build/fmt/Clippy/Rust 1514 passed、0 failed、36 ignored（110 目标）；原候选 Web Node132/Vitest561/build 通过。`0980dd36` 加 DOM 测试的 Web Node134/Vitest570/build 通过（Vitest 最终完整单 worker 重跑）。 | **阶段门未通过。** 新 revision 的 Rust/完整性能矩阵仍需最终重验；技术项未齐，不进入所有者关闭确认。仅 Mac ARM64，不外推部署、FNOS/NAS 或第三方客户端。 |

Web 复核保留一次默认并行全量的 5 个首页轮播失败；新增 DOM 文件与首页文件分别隔离 17/17 通过，最终完整 Node134/Vitest570 在 `--maxWorkers=1 --minWorkers=1` 下通过，build 通过。并行波动原因未证实，不能把隔离通过当全量通过或直接归因主机负载。

## 10k 阻塞与必须先解决的规格冲突

10k SQLite ignored benchmark 用合成空 provider gate 检验在线工作独立性。scan 已 `COMPLETED/IDLE`、10000 项；本地 15 批 COMPLETED、31 PENDING、4 RUNNING，在线 39 QUEUED+1 RUNNING、3970 项。600 秒等待后报告 `local queue timed out while provider was blocked`；总测试 602.59 秒失败。不是 provider 网络慢的产品测量。

来源链：本地 completeness 已原子持久化缺失结果与 job，随后同步调用 `enqueue_fill_missing_job_id`；dispatcher 的 `sender.reserve().await` 等待容量 32 队列，provider 不释放就不消费后续队列，背压传回本地 worker。1k 仅 10 个 job，未达到这一容量边界。

正式规格 LUX-424 明确要求满队列对扫描提交方背压；阶段 23/LUX-303 又要求独立在线任务不阻塞本地 worker。这两项在 10k fixture 上不能同时成立。按 **AGENTS.md / Source of truth**，在改变公共调度/存储边界前已报告并请求项目所有者选择。建议修订自动入口：QUEUED 数据库记录作为真实队列、每库 dispatcher 通过有界通知和分页异步领取，管理员/计划入口的背压与完成等待继续保留；不使用无界内存或逐 job spawn。该修订与实现尚未发生。

## 最短剩余清单

1. 项目所有者选择上述冲突的合同；若采用建议，先补 >32-job、阻塞 provider 下 local drain 的失败回归，再最小修复；保留 dedupe、每库串行、shutdown、取消和完成通知语义。
2. 在可用的采样窗口，以最终 revision、相同 scan/pool 配置完成 1k/10k × 两后端 × LUX-270/LUX-304 三轮配对，记录默认配置差异另作对照；拒绝受外部 Cargo 干扰的整个配对。补齐 10k online 矩阵。
3. 对有效复测中仍超过 5% 的稳定回退做 phase/pool-acquire/HTTP/SQL 定位、调整和完整复测；不删跨终点慢请求，不把 null 当零。
4. 最终源码重跑相关 Rust/Web 全门，更新精确 SHA、结果和兼容性记录；按全局完成标准补需要的真实用户流程验证。
5. 技术项全部满足后，按 AGENTS.md Stage gates 等待项目所有者明确确认；当前不请求关闭、不进入下一阶段。

## 持久证据与保留状态

`/Volumes/Toshiba/mywork/Lux-stage23-evidence-20261010-32c158ec` 保存 raw/repair/accepted-diagnostic JSONL、manifest/二进制与源码 hash、逐请求时刻、配对日志、质量门、online 失败与 SQLite 阻塞快照。源码测量补丁里的合成数据库密码已脱敏；hash 清单区分原始与归档字节。

主工作区原有 AGENTS.md 修改及未跟踪内容、旧脏基线未动；测量候选保持 `32c158ec` 干净。新增 DOM/记录位于基于 `0980dd36` 的独立 worktree。重复编译干扰后，停止的仅是本会话等待中的采样 driver，没有终止其他会话的编译/测试；相同并发诊断未运行。旧不公平样本与每次失败仍保留，不能宣称性能阶段达标。
