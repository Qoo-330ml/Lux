# 启动期一次性修复的稳态成本（分析项 A4 后半）

`src/main.rs` 每次启动都会依次执行一批“历史数据修复/迁移”。这些步骤在数据已经修好之后
仍然会被调用，所以“稳态库”每次重启都要至少查询一遍。本文记录这些步骤的实测成本，
并给出结论：**不需要引入新的“版本化一次性登记”机制**，代码保持基线行为（`c4eaef89`
与 `3d3fd3bb` 引入的登记机制已按评审意见回退，见 §4.2）。

## 1. 结论

| 结论 | 内容 |
| --- | --- |
| 真正的重复成本在哪 | A4 前半（`9e194e3f`）已经给媒体源默认值归一化、剧集图片路径修复、旧日志迁移、旧平台身份键修复写了 `lux_meta` 完成标记，因此稳态下每一步只剩 **1 条 `lux_meta` 读取**（0.03–0.11 ms，无写入）。 |
| 稳态总量 | 启动路径（步骤 1–5）≈ **1.2 ms / 12 条语句**；后台两步 ≈ 0.3 ms / 6 条。其中唯一有写语句的是步骤 5：8 条语句**全部**是写语句且**全部 `rows_affected=0`**（2 条空转 `UPDATE lux_meta` 认领 + 1 条 `UPDATE scan_manifest_roots` + 5 条空转 `DELETE`）。 |
| 最终决定 | **不引入版本登记机制**。唯一“合格”的步骤 5 登记后只能省掉 2 条空转 `UPDATE lux_meta`（该步 8 → 7 条语句，耗时差在噪声内），而这套跨进程认领协议会带来两类恢复/脏标记缺陷（§4.2），收益与风险不成比例。 |
| 因此改了什么 | 生产代码：**什么都没改**。本仓库只多了这份文档，以及 `src/storage/database_cleanup.rs` 里一个 `#[ignore]` 的测量夹具（`startup_repair_measurements`，只调用现有公开方法，不属于门禁测试，用于复现本文数字）。 |
| 保持原样的步骤 | 步骤 1–4、6、7 均已由既有标记/游标短路（每步 1 条查询、零写入）；步骤 5 的兜底载荷清理必须每次启动都跑（§4.3）。 |

> 数值说明：A4 前半没有标记时，本夹具上剧集图片路径修复要 **1242 ms**、媒体源默认值归一化
> 要 **196 ms**、旧身份键修复要 **3423 ms**（5 万条语句）。启动期真正昂贵的是这些一次性工作，
> 它们已经被登记过；剩下能省的只有步骤 5 的 2 条空转写。

## 2. 测量方法

### 2.1 夹具

用仓库自带的规格级夹具生成 66,000 条媒体：

```bash
# 1) 创建并迁移空库（测量用例本身就会 connect + migrate）
LUX_STARTUP_REPAIR_FIXTURE_DIR=/tmp/lux-startup-measure/config \
  cargo test --release --locked --lib \
  storage::repository::database_cleanup::startup_repair_measurements -- --ignored --nocapture

# 2) 播种 spec-target 目录（10,000 电影 + 1,000 剧集 × 50 集）
python3 tools/catalog-fixture/generate.py \
  --catalog-db /tmp/lux-startup-measure/config/lux.db \
  --spec-target --media-root /tmp/lux-startup-measure/media

# 3) 清掉第 1 步写下的完成标记，让下一次运行真正执行修复
sqlite3 /tmp/lux-startup-measure/config/lux.db "
  DELETE FROM lux_meta WHERE key IN (
    'audit_events_config_log_migration_v1','identity_stability_repair_v1',
    'scan_job_events_config_log_migration_v1',
    'startup_repair_episode_image_paths_v1','startup_repair_media_source_defaults_v1');
  UPDATE lux_meta SET value='PENDING' WHERE key='database_lifecycle_cleanup_v1';
  ANALYZE;"

# 4) 同一个用例内先跑一遍“预热”（完成修复并写下标记），再测量第二遍（稳态）
LUX_STARTUP_REPAIR_FIXTURE_DIR=/tmp/lux-startup-measure/config \
  cargo test --release --locked --lib \
  storage::repository::database_cleanup::startup_repair_measurements -- --ignored --nocapture
```

夹具规模（用例开头打印）：`media_items=66000`、`media_sources=66000`、
`filesystem_entries=66000`、`item_images=132000`、2 个库。

### 2.2 用例

`src/storage/database_cleanup.rs` 里的 `startup_repair_measurements`（`#[ignore]`，只手动跑，
不属于门禁测试）。它按 `main.rs` 的顺序调用每个步骤两次：

- **warmup**：第一次执行修复本身（数据量大时很慢，附表只作参考）；
- **steady**：第二次执行，即“已经修好的库”每次重启都要付的成本。

测量项：

- **耗时**：`std::time::Instant` 包住单步调用；
- **语句数**：`Database::query_count()`（`#[cfg(test)]`）；
- **写入**：用例安装了 `sqlx::query=debug` 的 `tracing` 订阅器，按 `== <pass> / <step>` 标记切分
  语句日志，凡是以 `INSERT`/`UPDATE`/`DELETE` 开头的语句即计为一次写入（含 `rows_affected=0`
  的空转写语句——这类语句同样要拿写锁、开事务）。

### 2.3 环境

- 机器：`arm64`（`uname -m`），macOS，`--release` profile，SQLite 单文件库（`lux.db`）。
- 测量窗口内本机还有其他 `cargo build/test` 并行运行，因此**绝对耗时是偏高的上限参考**；
  语句数与写入数是确定值，与机器负载无关。
- 这些数字只描述本机 + 该夹具，不能当作 NAS/x86 的性能结论。

## 3. 测量结果

### 3.1 逐步数据

| # | 步骤 | 稳态耗时 | 稳态语句数 | 稳态写入 | 预热耗时（修复本身） | 预热语句数 |
| --- | --- | ---: | ---: | --- | ---: | ---: |
| 1 | `migrate_legacy_scan_job_events_to_logs` | 0.06 ms | 1 | 无 | 2.18 ms | 4 |
| 2 | `migrate_legacy_audit_events_to_logs` | 0.11 ms | 1 | 无 | 0.60 ms | 4 |
| 3 | `repair_episode_image_path_conflicts` | 0.05 ms | 1 | 无 | **1242 ms** | 3 |
| 4 | `normalize_media_source_defaults` | 0.05 ms | 1 | 无 | **196 ms** | 3 |
| 5 | `run_database_lifecycle_cleanup` | 0.94 ms | 8 | **8**（全部 `rows_affected=0`：2×`UPDATE lux_meta` 认领、1×`UPDATE scan_manifest_roots`、5×`DELETE`） | 2.33 ms | 13 |
| 6 | `LibraryScanner::repair_legacy_identity_keys`（后台） | 0.03 ms | 1 | 无 | **3423 ms** | **50,003** |
| 7 | `scan_fingerprint_migration::start`（后台） | 0.26 ms<br>（仅当直接驱动其状态查询时） | 5 | 2（`INSERT … ON CONFLICT DO NOTHING` 建状态行） | 1.17 ms | 5 |
| | **启动路径合计（1–5）** | **≈ 1.2 ms** | **12** | **8**（全部来自步骤 5，且全部不影响任何行） | ≈ 1443 ms | 27 |
| | 后台合计（6–7） | ≈ 0.3 ms | 6 | 2 | ≈ 3424 ms | 50,008 |

补充说明：

- 步骤 3/4/6 的稳态只剩 1 条查询，正是因为 A4 前半写下的
  `startup_repair_episode_image_paths_v1`、`startup_repair_media_source_defaults_v1`、
  `identity_stability_repair_v1` 标记把它们短路了；没有这些标记时，本夹具上它们分别要
  **1242 ms / 196 ms / 3423 ms**。这就是这些标记存在的意义，也是本次不再动它们的理由：
  **登记不能让它更快，只会多一条查询**（见 §4.1）。
- 步骤 6 的预热语句数是 50,003：旧身份键修复**逐条**查询每个 EPISODE 的层级，5 万集就是 5 万条
  语句。它在后台 `tokio::spawn` 里跑，不阻塞 HTTP 监听，但确实是本机上最大的一次性启动修复。
- 步骤 7 在 `main.rs` 里只是 `tokio::spawn`（0 条语句、0 ms 启动阻塞成本）；它真正的状态查询在
  60 s 延迟后由后台 worker 执行，之后每 60 s 复查一次各库的游标。
- 步骤 5 的 8 条写语句分成两段：2 条 `UPDATE lux_meta` 是认领协议（标记已 `COMPLETED` 时
  `rows_affected=0`，与数据无关的纯粹重复成本），其余 6 条是兜底载荷清理
  （`UPDATE scan_manifest_roots` + 5 条 `DELETE`，稳态下同样 0 行，见 §4.3）。

### 3.2 判定规则

任务约定的判据：**只有“稳态耗时 > 50 ms 或有写入”且“结果幂等”的步骤**才值得登记。
按此规则筛出的候选与最终处理：

| 步骤 | 稳态 > 50 ms | 稳态有写入 | 幂等 | 处理 |
| --- | --- | --- | --- | --- |
| 1 旧扫描事件迁移 | 否（0.06 ms） | 否 | 是 | 保持原样（已有标记） |
| 2 旧审计事件迁移 | 否（0.11 ms） | 否 | 是 | 保持原样（已有标记） |
| 3 剧集图片路径修复 | 否（0.05 ms） | 否 | 是 | 保持原样（已有标记，见 §4.1） |
| 4 媒体源默认值归一化 | 否（0.05 ms） | 否 | 是 | 保持原样（已有标记，见 §4.1） |
| 5 数据库生命周期清理 | 否（0.94 ms） | **是**（8 条语句全是写语句，其中 2 条是认领协议） | 是 | **评估后半途放弃登记**（§4.2） |
| 6 旧身份键修复 | 否（0.03 ms） | 否 | 是 | 保持原样（已有标记） |
| 7 扫描指纹迁移 | 否（0 ms 启动阻塞） | 否（后台） | 是 | 保持原样（后台游标状态） |

也就是说：按判据只有步骤 5 入围，但入围≠值得实现——见下一节的收益/风险对比。

## 4. 决策明细

### 4.1 为什么步骤 3、4 不复用新机制

它们已经是“一次性”：标记缺失时才执行。若把标记换成新格式的 `startup_repair:<name>` 键，
升级后的**第一次启动会重跑一次**，本夹具上就是多付 1242 ms + 196 ms，而稳态成本
（1 条查询）没有任何变化。**登记不能让它更快**，只会让稳态多一条查询，所以保持原样。

### 4.2 为什么最终没有登记步骤 5（含评审发现的两处缺陷）

曾实现的方案：`Database::run_startup_repair_once(name, version, repair)`，把已完成版本号写在
`lux_meta.startup_repair:<name>`，用 `RUNNING:<ts>` 做跨进程认领，认领成功才执行、成功后才写版本。
指标上它确实成立：

- 收益：步骤 5 稳态 8 条语句 → 7 条，省掉 2 条空转 `UPDATE lux_meta`（实测该步耗时
  0.94 ms / 0.73 ms / 2.58 ms，差异在噪声内，等于约 2 条语句的成本）；
- 代价：一套跨进程认领协议（认领/释放/过期），以及下面两个恢复缺陷。

评审指出的两处真实缺陷（均已复现确认）：

1. **[P2] 崩溃后不再续跑**：认领写入 `RUNNING:<ts>` 后进程被杀死，下次启动读到“未过期”的认领
   就直接返回“已完成”，既不再执行修复体，也不会走到 `reset_interrupted_database_cleanup`
   （`database_cleanup.rs:430-432` 的既有注释承诺“failed or interrupted run can be retried by the
   next container start”）。1 小时的过期窗口只是把“下次启动续跑”变成“最长一小时后才续跑”，
   对一次可能很长的清理来说等于丢掉续跑能力。
2. **[P2] 脏认领导致永久静默跳过**：认领值若不是整数（例如 `RUNNING:abc`），解析失败被当作 0
   而判为“已过期”；但清除语句是按重新拼出的 `RUNNING:<解析值>` 精确比较 `value` 的，匹配不到；
   随后的认领 `UPDATE … WHERE value NOT LIKE 'RUNNING:%'` 又把这个脏值挡住 → 认领永远拿不到、
   版本号永远写不进去、修复永远不执行，而且没有任何日志。

因为唯一收益只是 2 条空转写（≈ 0.7 ms），这两个风险不值得承担，**决定不引入该机制**；
`c4eaef89`（helper + 测试）与 `3d3fd3bb`（接入）已回退，生产代码回到基线行为。

**备忘：将来若真需要登记，必须先覆盖这些点**

- 认领必须能与“本次启动批次”绑定（例如认领值带启动标识，只允许同一批次继续或清除），
  绝不能把“别人的认领”当成“已完成”；
- 不可解析/超期的认领必须**直接清除并重跑**（而不是解析成 0 后按字符串比较），且清除与认领
  在同一条语句里完成或用可判定的方式串行化；
- 认领状态不确定时返回“未完成”，绝不返回“已完成”；
- 先量化收益：只有当某个步骤稳态 > 50 ms（或有大量实写）时才值得引入额外状态机；
  省 2 条空转语句不值得。

### 4.3 兜底载荷清理为什么必须保持每次启动都跑

`run_database_lifecycle_cleanup` 里有两种不同性质的工作：

1. **一次性生命周期迁移**：`claim_database_cleanup` 拿到 `PENDING` 后执行
   `perform_database_cleanup`（清理已完成任务的路径/对账条目、汇总终态扫描任务等）。标记落到
   `COMPLETED` 后这段逻辑不再执行，但每次启动仍要先跑 2 条空转 `UPDATE lux_meta`（重置 + 认领）
   ——这就是 §4.2 里评估过、最终决定保留的那 0.94 ms。
2. **兜底载荷清理** `cleanup_completed_scan_manifest_payloads`：删除已完成扫描清单的
   delta/entry/seen_path/目录/本地元数据批次。扫描后处理
   （`scanner/jobs/postprocessing.rs`）在每个任务完成时本来就会调用它；启动时再跑一次是为了接住
   “进程在写完任务状态后被中断”留下的残留。它每次启动都可能发现新残留，**不是一次性工作**，
   登记会让残留一直留到下一次扫描。→ 任何未来的登记方案都不应把它纳入。

### 4.4 后台两步不登记

- 步骤 6 `repair_legacy_identity_keys` 在 `tokio::spawn` 里跑，不阻塞监听，且已有
  `identity_stability_repair_v1` 标记。
- 步骤 7 `scan_fingerprint_migration` 用 `scan_fingerprint_migration_state.completed_at` 做逐库
  游标，启动时只是 `tokio::spawn`，没有阻塞成本。

## 5. 现状与门禁

- 生产代码与基线 `test` 一致：`git diff test -- src/` 只有 `src/storage/database_cleanup.rs` 末尾
  那个 `#[cfg(test)] mod startup_repair_measurements`（`#[ignore]` 的测量夹具，只调用现有公开
  方法，不参与门禁测试）。`src/main.rs` 无改动。
- 与修复相关的既有测试照常通过，例如
  `cargo test --locked --lib -- storage::repository::database_cleanup database_lifecycle_cleanup`。
- 本文涉及的门禁：

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
python3 scripts/check-backend-branches.py
```

（`database_cleanup` 是 `storage::repository` 的子模块，测试全名是
`storage::repository::database_cleanup::…`；按 `storage::database_cleanup` 过滤会匹配 0 个用例。）

## 6. 未做与限制

- **没有 PostgreSQL 数据**：本机只有 SQLite 夹具。语句数与写入数在两个后端上一致（同一段 SQL、
  同一组占位符），绝对耗时未在 PostgreSQL 上复核。
- **耗时是上限参考**：测量窗口内有其他 `cargo` 构建并行运行；结论建立在语句数/写入数上。
- **没有把步骤 5 的 2 条空转 `UPDATE` 优化掉**：如果要省掉它们，安全的做法是在
   `run_database_lifecycle_cleanup` 里先读一次 `database_lifecycle_cleanup_v1`，已 `COMPLETED`
  就直接跳到兜底清理（1 条读换 2 条写，且不引入新的状态与认领）。这属于“省 1 条语句”的微优化，
  收益更小，本次也没有实施；记录在此供后续参考。
