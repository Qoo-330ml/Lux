# 启动期一次性修复：稳态成本测量与登记（分析项 A4 后半）

`src/main.rs` 每次启动都会依次执行一批“历史数据修复/迁移”。这些步骤各自在修复完成后
仍然会被调用，所以“已经修好的库”每次重启都要至少查询一遍。本文记录这些步骤在**稳态库**
（所有修复都已完成）上的实测成本，并据此决定哪些步骤值得加“版本化一次性登记”。

## 1. 结论摘要

| 结论 | 内容 |
| --- | --- |
| 真正的重复成本在哪 | A4 前半（`9e194e3f`）已经给媒体源默认值归一化、剧集图片路径修复、旧日志迁移、旧平台身份键修复写了 `lux_meta` 完成标记，因此稳态下每一步只剩 **1 条 `lux_meta` 读取**（0.03–0.11 ms，无写入）。 |
| 唯一仍有稳态写入的步骤 | `run_database_lifecycle_cleanup`：每次启动 8 条语句，**全部是写语句且全部 `rows_affected=0`**（2 条空转 `UPDATE lux_meta` 认领 + 1 条 `UPDATE scan_manifest_roots` + 5 条空转 `DELETE`）。 |
| 本次登记了什么 | 只登记 `run_database_lifecycle_cleanup` 的**认领 + 一次性清理**部分：改用 `startup_repair:<name>` 版本标记后，稳态语句 8 → 7，两条认领用的空转 `UPDATE lux_meta` 变成 1 条版本标记读取（`lux_meta` 写语句 2 → 0）。 |
| 保持原样的步骤 | 其余全部步骤（见 §4）。它们已经由既有标记短路，稳态耗时 < 0.3 ms 且无写入，**登记只会多一条查询**，不产生收益。 |
| 为什么不登记 `cleanup_completed_scan_manifest_payloads` | 它虽然每次启动都有 1 条写入，但那是**每次启动都必须再跑一次**的兜底清理（扫描后处理失败/进程中断时会遗留载荷），不是“一次性”工作，登记会改变行为。 |

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
  语句数、写入数、以及“稳态只剩 1 条查询”这一结论与机器负载无关。
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
| | **启动路径合计（1–5）** | **≈ 1.2 ms** | **12** | 3 | ≈ 1443 ms | 27 |
| | 后台合计（6–7） | ≈ 0.3 ms | 6 | 2 | ≈ 3424 ms | 50,008 |

补充说明：

- 步骤 3/4/6 的稳态只剩 1 条查询，正是因为 A4 前半写下的
  `startup_repair_episode_image_paths_v1`、`startup_repair_media_source_defaults_v1`、
  `identity_stability_repair_v1` 标记把它们短路了；如果没有标记，本夹具上它们分别要
  **1242 ms / 196 ms / 3423 ms**，这就是这些标记存在的意义（也是本次没有再把它们“重新登记”
  一遍的理由：登记不能让它更快，只会多一条查询）。
- 步骤 6 的预热语句数是 50,003：旧身份键修复**逐条**查询每个 EPISODE 的层级，5 万集就是 5 万条
  语句。它在后台 `tokio::spawn` 里跑，不阻塞 HTTP 监听，但确实是本机上最大的一次性启动修复。
- 步骤 7 在 `main.rs` 里只是 `tokio::spawn`（0 条语句、0 ms 启动阻塞成本）；它真正的状态查询在
  60 s 延迟后由后台 worker 执行，之后每 60 s 复查一次各库的游标。
- 步骤 5 的 8 条写语句里，2 条 `UPDATE lux_meta` 是认领协议（标记已 `COMPLETED` 时 `rows_affected=0`，
  纯粹的重复成本），其余 6 条属于兜底载荷清理（`UPDATE scan_manifest_roots` + 5 条 `DELETE`，
  同样 `rows_affected=0`，见 §4.2）。

### 3.2 登记后的稳态（同一个夹具、同一条路径）

| 项目 | 登记前 | 登记后 |
| --- | --- | --- |
| 步骤 5 稳态语句数 | 8 | **7** |
| 其中 `lux_meta` 写语句 | 2（重置 + 认领，均 `rows_affected=0`） | **0**（改为 1 条版本标记读取） |
| 其中兜底载荷清理（保留，每次启动都跑） | 6（1×`UPDATE scan_manifest_roots` + 5 条空转 `DELETE`） | 不变 |
| 步骤 1–4、6 稳态 | 每步 1 条 `lux_meta` 读取 | 不变 |

登记前后的耗时对比在本机噪声内（该步骤登记前一次 0.94 ms，登记后两次 0.73 ms / 2.58 ms；机器上
同时有别的 `cargo` 构建在跑），所以判断依据是**语句数与写入数**（确定值），不是毫秒差。

### 3.3 判定规则

任务约定：**只有“稳态耗时 > 50 ms 或有写入”且“结果幂等”的步骤**才值得登记。按此规则：

| 步骤 | 稳态 > 50 ms | 稳态有写入 | 幂等 | 是否登记 |
| --- | --- | --- | --- | --- |
| 1 旧扫描事件迁移 | 否（0.06 ms） | 否 | 是 | 否（已有标记） |
| 2 旧审计事件迁移 | 否（0.11 ms） | 否 | 是 | 否（已有标记） |
| 3 剧集图片路径修复 | 否（0.05 ms） | 否 | 是 | 否（已有标记，见 §4.1） |
| 4 媒体源默认值归一化 | 否（0.05 ms） | 否 | 是 | 否（已有标记，见 §4.1） |
| 5 数据库生命周期清理 | 否（0.94 ms） | **是（8 条语句全是写语句，其中 2 条是认领协议）** | 是 | **是**（认领 + 一次性清理部分） |
| 6 旧身份键修复 | 否（0.03 ms） | 否 | 是 | 否（已有标记） |
| 7 扫描指纹迁移 | 否（0 ms 启动阻塞） | 否（后台） | 是 | 否（后台游标状态） |

## 4. 决策明细

### 4.1 为什么步骤 3、4 保持原样，不迁到统一登记

它们已经是“一次性”：标记缺失时才执行。如果把标记换成新格式的 `startup_repair:<name>`
键，升级后的**第一次启动会重跑一次**，本夹具上就是多付 1242 ms + 196 ms，而稳态成本
（1 条查询）没有任何变化。按“对不值得登记的步骤保持原样”，这里保留既有标记。

### 4.2 为什么步骤 5 只登记“认领 + 一次性清理”，不登记兜底清理

`run_database_lifecycle_cleanup` 实际包含两件事：

1. **一次性生命周期迁移**：`claim_database_cleanup` 拿到 `PENDING` 标记后执行
   `perform_database_cleanup`（清理已完成任务的路径/对账条目、汇总终态扫描任务等）。
   标记落到 `COMPLETED` 后这段逻辑永不再执行，但每次启动仍要先跑 2 条空转 `UPDATE lux_meta`
   去“重置 + 认领”。→ **登记**，稳态改为 1 条版本读取。
2. **兜底载荷清理** `cleanup_completed_scan_manifest_payloads`：删除已完成扫描清单的
   delta/entry/seen_path/目录/本地元数据批次。扫描后处理（
   `scanner/jobs/postprocessing.rs`）在每个任务完成时本来就会调用它；启动时再跑一次是为了
   接住“进程在写完任务状态后被中断”留下的残留。它每次启动都可能发现新残留，
   **不是一次性工作**，登记会让残留一直留到下一次扫描。→ **保持原样**，稳态仍是
   6 条语句（1 写 + 5 空转删除，约 0.5 ms）。

### 4.3 后台两步不登记

- 步骤 6 `repair_legacy_identity_keys` 在 `tokio::spawn` 里跑，不阻塞监听，且已有
  `identity_stability_repair_v1` 标记。
- 步骤 7 `scan_fingerprint_migration` 用 `scan_fingerprint_migration_state.completed_at` 做逐库
  游标，启动时只是 `tokio::spawn`，没有阻塞成本。

### 4.4 为什么 `main.rs` 的调用点没有改动

`repair_historical_data` 仍然按原顺序调用同样三个函数，日志分支一字未动：唯一符合登记条件的
步骤是 `run_database_lifecycle_cleanup`，而它的认领逻辑在 `storage` 内部，登记也就落在
它的实现里（§4.2）；其余步骤已经自带标记，从 `main.rs` 再包一层只会多一条查询。`main.rs` 里
只补了一段文档注释，说明“新修复应当走 `run_startup_repair_once`，而不是再写一个一次性标记”，
避免下一个修复又从头加一套 ad-hoc 标记。

## 5. 版本化一次性登记机制

实现在 `src/storage/database_cleanup.rs`：

```rust
Database::run_startup_repair_once(name, version, || async { ... }).await
// Ok(Some(value)) 本次执行并且已写入版本标记
// Ok(None)        跳过（已完成 / 另一进程正在执行），或认领成功但修复无事可报告（也写标记）
// Err(error)      修复失败，认领被释放、不写标记，下次启动重试
```

- 键：`lux_meta.startup_repair:<name>`，值为**已完成的版本号（整数）**。已记录版本 ≥ 传入版本
  即跳过；把代码里的版本号 +1 就强制重跑。不新增表、不写迁移。
- 认领：沿用 `claim_database_cleanup` 的原子写法——先
  `UPDATE lux_meta SET value = 'RUNNING:<unix_ts>' WHERE key = ? AND value NOT LIKE 'RUNNING:%'`，
  没抢到再 `INSERT … ON CONFLICT(key) DO NOTHING`，只有 `rows_affected = 1` 的那次执行修复。
  并发启动时只有一方认领成功，另一方返回 `Ok(None)` 且零写入。
- 崩溃恢复：认领值带时间戳，超过 `STARTUP_REPAIR_CLAIM_STALE_SECONDS`（1 小时）视为上次进程
  已死，清掉后重试。普通失败（返回 `Err`）会**立即**释放认领，不等过期。
- 只有**赢得认领**的那一方才会写标记；`Err` 或认领被抢占都不写。闭包返回 `Ok(None)` 表示
  “拿到了独占认领、但该修复的状态已经完成（例如更早的标记已经做过这件事）”，同样算完成，
  所以也会记录版本；需要下次重试的场景必须返回 `Err`。
- 旧标记兼容：`startup_repair:...` 的值如果不是整数（例如旧的 `COMPLETED`/`1`），视为“未完成”
  并重跑一次，而不是让启动失败。
- 所有 SQL 只用 `?` 占位符，由现有 `Database::query` 适配 SQLite/PostgreSQL，
  **不新增 `DatabaseBackend::` 分支**（`scripts/check-backend-branches.py`）。

## 6. 测试

`src/storage/database_cleanup.rs` 的 `startup_repair_tests`：

| 用例 | 覆盖 |
| --- | --- |
| `startup_repair_runs_once_and_records_the_completed_version` | 标记缺失 → 执行并写入版本；再跑一次 → 跳过、修复体零查询、只花 1 条标记读取 |
| `startup_repair_reruns_after_the_version_increases` | 版本提升 → 重跑；旧版本请求不会回退已记录的版本 |
| `startup_repair_keeps_no_marker_when_the_repair_fails` | 失败 → 不写标记、认领释放；下次启动重试成功 |
| `startup_repair_records_the_version_when_the_repair_has_nothing_to_report` | 认领成功但无事可报告（旧标记已做过）→ 同样记录版本 |
| `concurrent_startup_repairs_claim_the_repair_once` | 两次启动同时认领 → 只有一方执行，另一方 `Ok(None)` |
| `a_claim_left_by_a_dead_process_is_retried` | 超过 1 小时的 `RUNNING` 认领视为崩溃遗留 → 重跑 |
| `a_live_claim_from_another_process_is_skipped` | 新鲜 `RUNNING` 认领 → 跳过且不删除别人的认领 |
| `an_unreadable_marker_is_treated_as_not_completed` | 旧格式/脏值（`COMPLETED`）→ 视为未完成并重跑，不让启动失败 |

“跳过时零查询”的说明：跳过必须读一次标记（`SELECT value FROM lux_meta WHERE key = ?`），
所以跳过成本是**恰好 1 条语句**，测试断言 `query_count() == 1` 且修复闭包一次都没被调用。
修复本身零查询，这就是能省下的全部。要真正做到 0 条语句得把标记缓存在进程内存里，而每个修复
在每个进程生命周期里只调用一次，缓存不会有收益。

门禁用例（不加 `--all-targets`）：

```bash
cargo test --locked --lib storage::repository::database_cleanup
```

（`database_cleanup` 是 `storage::repository` 的子模块，测试全名是
`storage::repository::database_cleanup::…`。）

## 7. 风险与未做

- **升级后一次性成本**：`run_database_lifecycle_cleanup` 换成新键后，任何数据库升级到本版本后
  第一次启动都会再走一遍“重置 + 认领”并写下版本标记。此时 `database_lifecycle_cleanup_v1`
  已经是 `COMPLETED`，两条 `UPDATE` 都是空转，`perform_database_cleanup` 不会执行
  ——实测这一次是 **12 条语句**（旧实现同场景是 13 条），之后回到 7 条。其余步骤沿用旧标记，
  行为与日志不变。
- **崩溃后的重试延迟**：进程被 `kill -9` 时认领值留在库里，最长 1 小时后（或该值被下次启动
  判定过期时）才会重跑；普通错误路径没有这个延迟。
- **并发多进程**：两个进程同时启动时只有一方执行修复；另一方跳过。极端情况下（一方认领后
  立即崩溃）修复会顺延，但没有数据损坏风险，因为所有已登记步骤本身都是幂等的。
- **未测量 PostgreSQL**：本机只有 SQLite 夹具。语句数与写入数在两个后端上一致（同一段 SQL、
  同一组占位符），绝对耗时未在 PostgreSQL 上复核。
