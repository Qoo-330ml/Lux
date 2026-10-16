# 第六项：NOT EXISTS 与扫描查询优化方案

日期：2026-10-16。状态：LUX-561 实施中。

分支：`codex/not-exists-antijoin-optimization`，基线：`test@1e7c2ea8`。
范围来自用户指定的 `cpu-hotspot-analysis.md` 第六项；附件中的建议作为待验证假设。
方案基于 `LUX-DEVELOPMENT.md` 中的 LUX-561 登记；以下增量均属于该任务，不提前推进其他阶段。

## 1. 结论与选择

推荐先完成两个改动：**完成检查用存在性判断；删除候选按单个 root 和 path 游标读取，并复用现有索引。**
保留具有索引支持的 `NOT EXISTS`。第一版不增加索引、不新增字段、不改变数据库关系。

附件所述“可能没有使用最优索引”尚不能证明线上存在全表扫描，更不能量化它对 300% CPU 的贡献。
本轮源代码、完整 SQLite migration 和合成实验的证据表明，反连接的内层已经能够精确查索引；主要可优化的是外层重复枚举、排序和不必要的完整计数。

| 附件建议 | 当前事实 | 本方案决策 |
|---|---|---|
| 给 `scan_manifest_observations(manifest_id, relative_path)` 建索引 | 当前无此表；format 3 使用 `scan_manifest_seen_paths`，旧格式使用 `scan_manifest_entries` | 使用实际表名和完整的 manifest/root/path 键；不创建重复索引 |
| `filesystem_entries(root, generation, is_missing)` | 当前已有 root/path 唯一索引及 live FILE 的 root/id 部分索引；原始 `!=` 查询在实验中仍只以 root 作为查找边界 | 不直接落地附件索引；必要时单独比较 generation 范围查询与索引写入成本 |
| 优化 `NOT EXISTS` | seen/delta 均命中完整复合键；旧 observation 可按主键倒序取最新一条 | 保留反连接和最新 observation 语义 |
| 消除全表扫描 | 实验中大表从 root 范围读取，并非每次遍历所有库；分页可能反复读取已处理的范围 | 重点消除重复枚举，保留正确性所需的一次完整检查 |

## 2. 当前调用路径与约束

直接相关文件：

- `src/storage/jobs/scan/manifest/diff/finish.rs`：LITE、PERSISTED v3、workflow 2/3 旧格式、legacy 四个 remaining-change 分支。
- `src/storage/jobs/scan/manifest/diff.rs`：完成协调、普通和目录删除候选、uncovered 检查。
- `src/application/scanner/jobs/manifest_index.rs`：LITE、目录、uncovered、legacy 的候选分页与 delta 写入。
- `src/storage/filesystem.rs`：直接扫描入口的整 root 缺失标记，以及 reconciliation 的精确路径标记。
- `src/storage/media_sources.rs`：缺失标记之后的媒体移除状态刷新，须单独计时。

当前新全量扫描以 LITE 为主；必须优先测量这条路径，同时保留旧任务恢复能力。
根据 ADR-044/045，LITE 不依赖逐目录持久 frontier；PERSISTED v3 仍只按已完成目录的直接文件子项产生相应候选。
不能把两条路径强行合并成同一套目录过滤规则。

现有关键索引：

| 表 | 键或索引 | 用途 |
|---|---|---|
| `filesystem_entries` | `UNIQUE(library_root_id, relative_path)` | 精确路径、固定 root 内的 path 范围与顺序读取 |
| `filesystem_entries` | `(library_root_id, id) WHERE entry_kind='FILE' AND is_missing=0` | 本地 metadata 回填；也可能被 diff 选中，但不提供 path 顺序 |
| `scan_manifest_seen_paths` | `PRIMARY KEY(manifest_id, library_root_id, relative_path)` | FILE presence 例外账本 |
| `scan_manifest_deltas` | `UNIQUE(manifest_id, library_root_id, relative_path)` | 排除已有 delta，包含终态 delta |
| `scan_manifest_entries` | `PRIMARY KEY(manifest_id, library_root_id, relative_path, observation_sequence)` | 最新 observation 与旧任务恢复 |
| `scan_manifest_directories` | `(manifest_id, library_root_id, state, relative_path)` | 完成目录枚举；相关 LIKE 匹配不能等同于 path 等值查找 |

迁移 `0134` 已删除与 observation 主键重复的索引。PostgreSQL 的 `0162` 还为 live FILE 提供了 `text_pattern_ops` 前缀索引。

必须保持以下语义：

1. root 完整性、root 身份复核、取消和运行状态 fencing，以及删除前 stat/基线 ID/fingerprint CAS。
2. generation、seen-path、已有 delta 三层排除；不能只用 generation 推断缺失，也不能只排除 PENDING delta。
3. format 2/legacy 取最新 observation；不能把“曾经观察到 FILE”当成“最新观察是 FILE”。
4. 直接目录子项、根目录文件、父目录已消失、历史 uncovered 路径，以及通配符转义规则。
5. delta/target/进度的事务原子性；finish 的检查、状态转换与 job fencing 仍位于原写事务。

## 3. 设计一：完成检查从 COUNT 改为 EXISTS

`finish_scan_manifest_diff` 只用 `remaining_changes > 0` 决定是否重试，不消费数量。
将 helper 改为明确返回 bool 的 `manifest_diff_has_remaining_changes`：

```sql
SELECT CASE WHEN EXISTS (
    SELECT 1
    -- 原分支的 FROM / JOIN / WHERE 条件原样保留
) THEN 1 ELSE 0 END
```

SQLite/PostgreSQL 经现有 SQLx Any 边界继续使用整数 0/1，避免假设两后端 bool 解码完全相同。
四个分支分别验证；legacy 的“正向 count + 反向 count”改为两个存在性判断的 OR。
真正用于报表和状态的 `unchanged_count`、`remove_count`、总数等仍精确计数。

收益边界：存在未处理差异时可以提前结束；没有差异时仍必须完成验证。
不宣称这一改动让无变化重扫变成 O(1)，也不承诺所有扫描顺序都能很早命中。

## 4. 设计二：固定 root 后进行 path 游标分页

### 4.1 问题

目录候选当前没有 path cursor；写入一页 delta 后再次从头读取，依赖 `NOT EXISTS(delta)` 跳过前面的结果。
LITE/普通候选虽有 `(root, path)` tuple cursor，也要验证实际计划是否将 path 变成索引范围。
`LIMIT 500` 只限制输出数量，不能保证只检查 500 行。

单独增加 tuple cursor 不足以解决问题：本轮实验仍选择 root/id 索引，并出现临时排序。

### 4.2 查询与协调方式

1. 在 storage 增加有上限的 COMPLETE root keyset page；只读取当前 Manifest 的 root 快照。
2. 每次候选查询固定一个 `library_root_id`，用 `relative_path > after_path` 和 `ORDER BY relative_path LIMIT ?`。
3. 保留 root 完整性门槛；将 root guard、Manifest/job generation 查找写成与候选 path 无关的子查询，避免主查询 JOIN 使优化器丢失有序范围读取。
4. seen-path 和 delta 继续使用完整键的 `NOT EXISTS`。
5. PERSISTED 目录页保持当前 128 条上限；跨 root 时按 root 分组、顺序处理。每个固定 root 的目录组有独立 path cursor，路径条件继续只匹配直接子项。
6. LITE/普通路径采用相同的固定 root 查询骨架，但保留各自的 generation/observation/uncovered 分支。
7. 每页 delta 写入成功后，cursor 才前进到最后一条候选；错误时不推进。重试从头枚举，由已持久化 delta 保持幂等，不新增持久 cursor 字段。

核心查询形态如下，目录过滤和格式过滤按原分支补入：

```sql
SELECT fe.library_root_id, fe.relative_path,
       fe.id AS base_filesystem_entry_id, fe.fingerprint AS base_fingerprint
FROM filesystem_entries fe
WHERE fe.library_root_id = ?
  AND fe.entry_kind = 'FILE' AND fe.is_missing = 0
  AND fe.relative_path > ?
  AND EXISTS (
      SELECT 1 FROM scan_manifest_roots root
      WHERE root.manifest_id = ? AND root.library_root_id = ?
        AND root.state = 'COMPLETE'
  )
  -- 保留本格式的 presence / observation / directory 条件
  -- 保留 seen-path 与 delta 的完整键反连接
ORDER BY fe.relative_path
LIMIT ?
```

预期访问路径是已有 root/path 唯一索引的 `(library_root_id=? AND relative_path>?)`，不使用生产 `INDEXED BY` 强制计划。
不要求“所有情况下绝不排序”；验收要结合实际检查行数和时间，区分有界结果排序与重复大范围读取。

### 4.3 并发与目录语义

- cursor 经过后若出现较小 path 的并发变化，事务内 finish 检查仍须发现未处理差异，返回 false，沿既有 `MANIFEST_DIFF_RETRY` 路径重新枚举。
- 取消、事务失败与重启必须保留既有幂等和恢复边界，不能因内存 cursor 跳过未提交工作。
- 不照搬递归 sidecar 查询的“祖先目录去重”：REMOVE 按直接子项判断，父目录不能替代子目录完成状态。
- 第一版保留目录 LIKE/转义及 uncovered 检查。直接改成 `parent_path` 等值查找或字典序前缀范围，会涉及大小写、字符排序和异常历史路径，收益不足以支撑本次额外复杂度。
- 不新增 parent 字段、生成列或整个媒体库的内存集合。

## 5. 设计三：整 root UPDATE 的处理

`mark_missing_filesystem_entries` 属于直接扫描入口；Manifest 主路径使用 delta/apply，reconciliation 使用精确路径批量更新。
因此不能把这条 UPDATE 的耗时直接归因到所有新全量扫描。

第一版保留 UPDATE 和同事务的媒体状态刷新。现有唯一索引已能限制到目标 root。
需要分别测量缺失 UPDATE、availability trigger、`refresh_removed_media_items_in_transaction`，防止优化错对象。
不能简单在 `rows_affected == 0` 时跳过刷新，因为这可能改变现有状态修复行为。

只有完整测量证明这一入口仍是热点时，才比较：

- 附件索引 `(root, generation, is_missing)`；
- 等值条件靠前的 `(root, is_missing, generation)`；
- 部分索引 `(root, generation) WHERE is_missing=0`，该入口同时处理目录，不能限定为 FILE；
- 在两后端的 `last_seen_generation NOT NULL` 约束下，将 `!=` 表达为两个互斥的 `<`/`>` 范围；比较 OR 和单条 UPDATE 内的 UNION 候选形态。

generation 是不透明字符串，`<`/`>` 仅用于表达不相等，不能解释成时间或新旧顺序。
范围比较、原比较和索引必须使用一致的排序语义；同一次 UPDATE 保持原子性和触发器行为。
原 `!=` 不保证使用 generation 范围，本轮候选索引实验即观察到仍只按 root 搜索。

默认不新增索引。只有双后端数据证明净收益才增加：建议门槛为目标热点阶段中位耗时至少改善 10%，且完整 discovery/扫描和前台请求 p95 均无超过 5% 的回退。
这些是本任务拟采用的验收门槛，不替代项目既有性能门。
generation 每轮会更新大量行，新增索引必须把更新成本、WAL、DB 大小和锁等待算进去。

若需要 migration：使用实施时下一个可用编号，为 SQLite/PostgreSQL 各写新增 migration；测试空库和真实旧 schema 升级，不改旧 migration 或 SQLx checksum。

## 6. 本轮实验及其边界

环境：macOS arm64、Python SQLite **3.51.0**、内存库；执行基线全部 165 个 SQLite migration（最高编号 0175）。
锁定依赖中的 SQLite 源码版本为 **3.51.3**，所以以下结果不能代替 SQLx 实测。
数据完全合成：两个 root 各 60,000 条 FILE，目标 Manifest 只覆盖一个 root；执行 ANALYZE。
计时为开发机观察值，不作为通过阈值。未运行 Cargo、生产数据库或 PostgreSQL。

观察到的访问路径：

- seen 和 delta 使用完整 `(manifest, root, path)` covering index。
- 附件 generation 索引配原 UPDATE，仍只有 `library_root_id=?` 搜索条件。
- 原目录分页使用 root/id 部分索引并出现临时排序。
- 固定 root、标量 path cursor、独立 guard 子查询的形态使用已有 root/path 唯一索引，且该 fixture 没有临时排序。
- fixture 只有一条 manifest 和 job 时，优化器选择小表 SCAN；这不能混同为大 filesystem 表全扫。

目录实验把目标 root 的 60,000 条记录设为旧 generation、单个大目录、全部待删除，每页 500，并实际插入 delta：

| 查询形态 | 页数 / 候选数 | 约 VM 指令数，含 delta 插入 |
|---|---:|---:|
| 原查询，反复从头读取 | 120 / 60,000 | 349,140,000 |
| 只加 tuple cursor | 120 / 60,000 | 294,482,000 |
| 固定 root + 标量 path cursor + 独立 guard，复用旧索引 | 120 / 60,000 | 10,920,000 |

以上仅检查候选枚举和 delta 插入；没有磁盘目录、SQLx 连接池、stat/CAS/apply、媒体源、availability trigger 的真实工作负载。
相同页数和行数也不能替代逐集合及并发正确性测试。
每 1,000 条 VM 指令采样一次，表中为近似值；最后一行约为首行的 1/32，不能外推为整次扫描或 NAS CPU 降低 97%。

完成检查实验：60,000 条均属当前 generation 时，COUNT 和 EXISTS 均约 420,000 VM 指令；存在首部未处理项时 EXISTS 能提前结束。
新增 generation 索引还可能改变行顺序，把该未处理项放到最后，因此“换 EXISTS 就一定很快”也不成立。

SQLx 基线已在 storage::repository::jobs::tests::manifest_diff_antijoin_benchmark 建立：SQLite 3.51.3、两个 60k root、每页 500。三轮 LITE tuple 候选中位数 3.91s / 121 wrapper calls；PERSISTED 目录候选加 delta 核验与写入中位数 16.24s / 1,561 calls。实际查询计划确认 seen/delta 使用完整键索引；filesystem 候选外层使用 root/id 部分索引并做临时排序，tuple cursor 未形成 path 范围。PERSISTED 方差较大，后续需拆候选读取和 delta 写入耗时并在交错 A/B 中复测。完整记录见 docs/PERFORMANCE.md。

## 7. 实施顺序、文件与验收

开始生产改动前，先用增量 C 的 fixture/harness 在原版本保存 SQLx 基线，后续每个增量沿用同一组数据比较。
性能改动先建立可复现的成本回归；涉及行为的改动先写红灯测试，再做最小改动；完成后原子提交。总任务完成时才跑全量门。

### 增量 A：存在性完成检查

预计文件：`src/storage/jobs/scan/manifest/diff/finish.rs`、`src/storage/jobs/scan/manifest/diff.rs`、`src/storage/jobs/tests.rs`、`docs/LUX-DEVELOPMENT.md`。

验收：四个分支的零差异/未完成差异/已有 delta 结果保持；legacy 两类差异均阻止提前完成；真实计数与取消 fencing 保持。
添加“未完成项靠前/靠后/不存在”的查询行为回归；性能检查不依赖时间脆弱断言。
最窄检查：`cargo test --locked --lib storage::repository::jobs::tests::manifest_diff_`，其中该前缀用于新增的相关库测试。

### 增量 B：固定 root 的有界候选枚举

预计文件：`src/storage/jobs/scan/manifest/diff.rs`、`src/application/scanner/jobs/manifest_index.rs`、`tests/scanning_jobs.rs`、`src/storage/jobs/tests.rs`、`docs/LUX-DEVELOPMENT.md`。

验收：LITE 和 PERSISTED 的候选集合与旧实现逐项相同；多 root 同名 path 无串库；单目录超过两页仍无遗漏、重复。
测试目录组跨 root、根目录、嵌套目录、`A`/`AB`、`%`/`_`/反斜杠/中文/大小写路径，以及 parent 消失和异常 uncovered 路径。
注入 cursor 之后发生的较小 path 并发写入，确认 finish 重试；覆盖取消、回滚、恢复和最新 observation。
最窄检查：先运行新增库测试，再运行 `cargo test --locked --test scanning_jobs`。

沿用该 target 中已有的安全回归，重点包括：

- `compact_manifest_removes_a_path_only_after_complete_root_and_absence_check`
- `compact_manifest_removal_checks_each_completed_directory_without_prefix_overlap`
- `manifest_uses_latest_directory_observation_when_diffing_file_paths`
- `manifest_confirms_missing_file_when_its_parent_directory_is_gone`
- `incremental_scan_seen_before_manifest_diff_is_not_marked_missing`
- `manifest_apply_does_not_overwrite_a_newer_incremental_filesystem_entry`
- `manifest_does_not_remove_entries_when_root_is_replaced_after_discovery`
- `manifest_removal_delta_rolls_back_missing_state_and_targets_atomically`

### 增量 C：性能矩阵与是否增加索引的决策

预计文件：`src/storage/jobs/tests.rs`、`tests/performance.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`。

新增显式 ignored 基准：`manifest_diff_antijoin_benchmark`，放在 `src/storage/jobs/tests.rs` 以直接执行真实 storage repository 方法，复用 SQLite/PostgreSQL 配置；完整扫描 A/B 复用 `tests/performance.rs` 的现有 fixture。两类测量都与旧版本使用同 fixture 比较。
至少 3 轮，交错运行基线/候选；记录中位数、范围和原始样本，固定数据及并发设置。

| 维度 | 必测组合 |
|---|---|
| 后端 | SQLx SQLite；真实 PostgreSQL，记录实际 server 版本和 collation |
| 规模 | 10k、60k、240k FILE；多个 root 与单大目录/多小目录 |
| 工作负载 | 无变化、少量变化、1% 删除、全删除、候选靠前/靠后 |
| 格式 | 默认 LITE；PERSISTED v3；旧 format/legacy 的正确性与代表性性能 |
| 状态 | 大量历史 missing、历史 delta、ledger 例外、不可用 root、全量/增量交错 |
| 缓存 | 首次连接/热连接分别报告；不能把首次连接称为磁盘冷缓存 |

每个窗口记录：SQL 次数、数据库执行和连接池等待、检查/返回行数或 VM 指令、sort、root/候选/目录规模、事务耗时、整轮吞吐、前台 API p95、DB/WAL 大小。
generation 索引候选还要测完整 discovery 的写入，不只测 SELECT。
SQLite 用实际 SQLx 版本的查询计划与可用的执行计数；PostgreSQL 使用 `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)`。
UPDATE 的 EXPLAIN ANALYZE 会执行写入，只允许在合成 fixture 内运行并回滚。

如果 UPDATE 没有独立热点证据，记录“不新增索引”的结论即可。
如果满足第 5 节门槛，再增加独立增量：`src/storage/filesystem.rs`、两后端各一份新增 migration、`tests/storage.rs`、`docs/LUX-DEVELOPMENT.md`，每次仍不超过 5 个文件。
media refresh/trigger 若是主要成本，只记录证据，本任务不顺手改变其修复或一致性合同。

### 增量 D：完成门与记录

文档记录语义兼容和证据边界：`docs/PERFORMANCE.md`、`docs/COMPATIBILITY.md`、`docs/LUX-DEVELOPMENT.md`。
全部 Rust 变化完成后运行：

```bash
# 先以系统挂载信息确认 /Volumes/Toshiba 是真实挂载卷，再记录 uname -m。
# 以下环境变量必须覆盖所有 Cargo 命令，包括前面各增量。
export CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target
cargo build --locked
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
```

若有 migration，定向增加 `cargo test --locked --test storage`；涉及直接扫描入口时增加 `--test scanner --test series_scanner --test mixed_scanner`。
项目或所属阶段要求完整检查时使用 `./scripts/check-all.sh`，将既有 Web/其他失败与本任务回归分别记录。
PostgreSQL 无可用服务时保持该后端未验证，不能把 SQL 文本检查记成 runtime 通过。
本机 ARM 结果、双后端测试与 FNOS/x86 实测分别报告；不推送、合并或部署作为本设计工作的隐含步骤。

## 8. 风险、回退与完成判定

主要风险是改变目录覆盖或并发 cursor 语义，其次是新增索引放大 generation 写入。
通过逐集合比对、取消/故障注入、finish 再检查，以及完整扫描 A/B 控制这些风险。
观察到规格冲突时，按 AGENTS.md 报告具体冲突，不自行改变公共模型或目录合同。

优先方案没有 schema 改动，可按增量回退；索引若最终独立加入，SQL 仍须兼容有无该索引，回退不删除用户数据。
不能通过关闭 root/CAS 检查、绕过触发器或删掉失败测试获取性能结果。

本轮设计交付完成条件：分支独立、查询与索引事实已核对、优化路径和文件明确、正确性/性能/迁移门明确。
后续实现完成条件：相关语义测试通过、完整 Rust 门结果如实记录、热点净收益有证据；没有收益的候选不保留。
不设“NOT EXISTS 数量减少”或“计划里不能出现任何 SCAN”作为目标。

## 9. 依据

- 项目：`AGENTS.md`、`LUX-DEVELOPMENT.md` 的 12.4 与全局完成标准，ADR-043/044/045。
- [SQLite 优化器与索引前缀规则](https://sqlite.org/optoverview.html)：等值前缀、范围、LIKE 和排序限制。
- [SQLite 查询计划说明](https://sqlite.org/eqp.html)：SCAN/SEARCH 的含义，以及计划文本会随版本变化，测试不应锁死整段输出。
- [SQLite 部分索引](https://sqlite.org/partialindex.html)：谓词匹配与维护成本。
- [PostgreSQL 多列索引](https://www.postgresql.org/docs/current/indexes-multicolumn.html)：各后端分别验证访问路径。
- [PostgreSQL EXPLAIN](https://www.postgresql.org/docs/current/using-explain.html)：实际执行与 buffers，ANALYZE 会执行语句。
