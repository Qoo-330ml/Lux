# ADR-044：全量扫描使用紧凑文件存在性清单

## 状态

已接受

## 日期

2026-09-25

## 背景

LUX-267 v2 已将新增/变化/重新出现文件的正向索引合并进 discovery 事务，6 万文件 SQLite/ARM64 release 首扫中位数约 2.85 秒。剩余成本主要位于正向索引事务；每个文件仍会在 `scan_manifest_entries` 写入 path、sequence、kind、size、mtime、device、inode 和 fingerprint，尽管提交后实际索引事实已在 `filesystem_entries`，缺失判断只需知道该 path 在本 root 的完整发现中出现过。

新设计必须在减少重复写入的同时保留目录 checkpoint 和崩溃恢复、全量/增量并发 CAS、完整 root 门槛、删除前安全路径复核，以及 SQLite/PostgreSQL 相同的核心语义。目标不只是减少计数器或 SQL 数量，而是在完整 6 万文件基准上实测总耗时和无变化重扫。

## 决策

- 新任务使用 `discovery_format_version=3`。既有受 CHECK 约束的 `workflow_version` 保持 2；新增独立 format 字段，避免 SQLite 为扩大旧 CHECK 而重建含外键的 `scan_manifests` 表。迁移 0135 将现有 manifest 的 format 默认设为 2；新任务明确使用 3。
- v3 新增 `scan_manifest_seen_paths(manifest_id, library_root_id, relative_path)`，复合主键保证每个 manifest/root/path 最多一行。成功正向索引和 fingerprint CAS 成功的稳定 unchanged 文件都通过 `filesystem_entries.last_seen_generation` 标记本次扫描，并以 `last_seen_change_kind` 保留 `NEW`/`CHANGED` 的 postprocessing 语义；ledger 只记录无法安全推进 generation 的已观察文件，例如准备不稳定或 CAS 冲突。它不重复保存 size、mtime、device、inode 或 fingerprint。
- root 与目录身份 observation 继续追加到 `scan_manifest_entries`。文件 stat/fingerprint 在 discovery 内存中用于二次安全校验、分类和基线 ID/fingerprint CAS；成功的正向文件事实写入当前 `filesystem_entries`/媒体索引。
- presence ledger、文件/媒体索引、目录 frontier 和进度必须在同一个有界事务提交或回滚。重复恢复同一目录通过 `ON CONFLICT DO NOTHING` 幂等。
- `scan_job_targets` 在所有索引与 REMOVE 确认完成、首页快照切换并发布索引完成事件后生成。Manifest 和每个 root 都保存 target-ready 标记；root 保存 keyset cursor。每页 target 写入与 cursor 前进同事务，最后 root 完成和全局 ready barrier 原子提交。worker 仅在全局 ready 后启动。迁移为既有 workflow/discovery format 设 ready 默认值；新 v3 Manifest 明确设为未就绪。
- 只有完整且身份仍匹配的 root，才对 `filesystem_entries` 中 generation 与本次扫描不同、且不存在 seen-path 记录的 FILE 产生 REMOVE 候选。每个删除仍需安全复核路径并按基线 entry ID/fingerprint CAS；不可用 root、取消或 I/O 错误不能删除。
- format 3 的 completed payload cleanup 按有界批次删除 seen-path 行并清空 target cursor；FAILED/CANCELLED 的 checkpoint 与 ledger 保留以供重试。没有公开 API、Webhook 或 Emby 合同变化。
- SQLite 与 PostgreSQL 共用复合主键、`ON CONFLICT DO NOTHING` 和有界批次；seen-path 写入每条 path 1 个 bind 加 2 个固定 bind，单条最多 997 行。成功正向索引和 CAS 成功的 unchanged 文件已设置当前 scan generation，因此不会重复插入 presence 行；只有不稳定或 CAS 冲突路径进入 ledger。

## 考虑过的方案

### 继续保存每个文件的完整 observation

这最接近 format 2，恢复直接，但对已原子提交进 filesystem/media 索引的 positive path 重复持久化 fingerprint 与 stat 字段，且 SQLite 每行需 8 个 bind。v3 的恢复边界是 ledger 与同事务目录 frontier，因此完整 file row 不再是安全删除所必需的信息。

### 仅用 `filesystem_entries.last_seen_generation` 做全量 sweep

这可省去单独 ledger，但取消/崩溃后重走未完成目录还需清理该 generation 下的部分路径，且必须避免清除并发增量版本。Format 3 复用 generation 标记表示本次事务成功提交的正向索引或 fingerprint CAS 成功的 unchanged 观察；准备不稳定和 CAS 冲突路径仍写入紧凑 ledger，以免把不完整观察误判为缺失。

### PostgreSQL COPY 或临时 staging table

这会让 SQLite 与 PostgreSQL 的事务、失败回滚和恢复语义分叉。只有两后端 runtime 基准证明通用有界 ledger 写入仍是瓶颈时，才另行评估。

## 后果与验证

- 新 schema 只追加 discovery format 字段和 seen-path 表；不重建 `scan_manifests`，不访问文件系统、不回填全库、不转换活动旧任务。
- ledger、目录 frontier 和正向索引共享批次事务，故 rollback 时不得留下 seen path；重启时已提交的 path/目录可幂等复用，未完成 root 仍不具备删除权。稳定 unchanged 的 generation 更新带 observed fingerprint CAS，冲突时转入 ledger；删除候选排除 ledger 中的路径。target materialization 只处理带本次 generation 和有效 change kind 的已索引文件，并通过 root identity check 避免路径被替换后推进游标。
- 验收覆盖 SQLite 空库与当前 schema 升级、format 2 活动 Manifest 保持、format 3 首扫/无变化重扫/删除/取消恢复/CAS 竞争/事务回滚/target readiness barrier/清理，以及三轮 60,000 文件 release benchmark。
- PostgreSQL migration 文本与核心 SQL 有自动化检查；只有运行 PostgreSQL integration tests 和实际 6 万文件 PostgreSQL 基准后，才能声称该后端 runtime 通过。ARM64/SQLite 数字不外推 NAS/x86_64。
