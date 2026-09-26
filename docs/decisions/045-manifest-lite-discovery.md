# ADR-045：新全量扫描使用内存目录 frontier

## 状态

已接受

## 日期

2026-09-26

## 背景

ADR-043 为全量扫描建立了可恢复的持久目录 frontier。它保证进程重启后可以从最后提交的目录继续，但每个目录发现批次都需要维护 frontier 行、状态和 checkpoint。对新 workflow 的 discovery format 3，这些目录行不再是删除安全或正向索引安全所必需的信息：文件事实已经在同一有界事务中写入 `filesystem_entries`/媒体索引，`last_seen_generation`、紧凑 seen-path ledger 和根路径状态承担本轮存在性与并发保护。

## 决策

- 新建全量扫描固定使用 `workflow_version=2`、`discovery_format_version=3`、`discovery_mode=LITE`。只有这个三元组启用 Lite；旧 Manifest 默认仍使用 `PERSISTED`。
- Lite 在创建任务时复制根路径列表。扫描开始后新增的媒体库根路径不加入当前任务，下一次扫描处理它。
- Lite 将待扫描目录保存在进程内 frontier 中，目录读取和事务提交按有界批次推进。同一 root 的目录合并读取和提交；子目录不写入 `scan_manifest_directories`，只保留 root observation、root 状态、正向索引、generation/seen-path 和删除候选所需的持久状态。
- 每个有界批次仍以单事务提交目录读取结果、二次 stat/fingerprint 通过的正向索引、`last_seen_generation`、presence ledger、计数和进度。事务失败或取消时，未提交目录不会进入完成状态。
- 服务重启后，未完成的 Lite 任务从任务创建时的 root 列表重新遍历 root。重复发现依靠 fingerprint、generation CAS、幂等 upsert 和删除前二次 stat 保持安全；旧 PERSISTED workflow 继续使用持久 frontier 恢复。
- 只有 root 完整且设备/inode 仍匹配时才允许缺失确认和 REMOVE；不可用、取消、I/O 错误和 root 替换仍禁止删除。首页快照、target-ready barrier、事件和 SQLite/PostgreSQL SQL 合同不变。

## 考虑过的方案

### 新扫描继续持久化每个目录 frontier

它能提供更精确的崩溃恢复点，但增加目录状态写入和提交往返。Lite 只在新扫描中放弃这个恢复粒度，旧任务仍保留原合同。

### 完全移除 Manifest

这会放弃当前的根覆盖门槛、稳定首页切换和全量/增量 CAS 合同。需要另行修改产品规格和公开完成语义，本 ADR 不采用。

## 后果与验证

- 新扫描重启后会从 root 重读已提交目录，可能重复文件系统读取；不会重写稳定正向索引或重复后处理目标。
- 新扫描减少 `scan_manifest_directories` 的逐目录 DML 和 frontier 查询；format 3 也不保存逐文件完整 observation，文件 stat 只在内存中参与准备和二次校验。
- `cargo test --locked --test scanning_jobs` 覆盖 Lite 根快照、无子目录 frontier、正向索引、取消、root 替换、删除保护和重扫语义。SQLite/PostgreSQL 性能数据只代表记录的本机和临时数据库，不外推 NAS/x86_64。
