# ADR-043：全量扫描使用持久化 Manifest

## 状态

已接受；文件级 observation 的存储方式由 ADR-044 修订，既有 discovery format 1/2 合同保留。

## 日期

2026-09-24

## 背景

全量扫描已经使用持久化目录/文件工作队列、文件指纹、短批次事务和缺失文件二次确认。工作队列只描述待做工作，不构成不可变的文件系统观察快照，也不能将扫描发现的差异与后来完成的增量扫描安全区分。首页快照又在完整后处理后才切换，导致“索引完成”和“后处理完成”的任务通知时点不清晰。

LUX-154、LUX-187、LUX-230 对目录发现、首页可见时点和快照刷新时机存在语义分歧。本 ADR 将它们统一为：扫描批次提交期间首页保留旧稳定快照；Manifest 索引和缺失确认完成后切换首页快照；probe、NFO、图片、封面和缩略图继续异步后处理。

## 决策

- 全量扫描拥有独立的持久化 Manifest 模型，包含 Manifest 生命周期、逐根路径覆盖状态、持久目录 frontier、不可变文件系统 observations 和版本化的 apply 策略。
- `filesystem_entries` 继续作为当前文件系统索引事实来源。Manifest 通过 `library_root_id + relative_path` 与它比较，不从 `media_items` 单独推导删除。
- Manifest observation 不原地覆盖。新版在发现批次内批量读取 `filesystem_entries` 基线，对新增/变化/重新出现文件再次 stat/fingerprint；在提交事务中对基线 ID 和 fingerprint 做 CAS，避免全量扫描覆盖并发增量结果。
- 正向索引融合到有界发现事务：observation/presence、文件系统/媒体索引、目录 frontier 和进度全部提交或全部回滚。正向 ADD/CHANGE 不再各自写入和更新一条持久 delta；批次提交本身就是可恢复 checkpoint。新版 file presence format 与 workflow version 分开版本化，细节见 ADR-044；升级前的活动 Manifest 保留原执行器和恢复合同。
- v3 的 SOURCE/ITEM postprocessing targets 不阻塞索引事务。索引及安全缺失确认完成后，target worker 按 root/path 游标批量物化 targets；每页 target 写入与 root 游标原子提交，最后一页同时设置所有 root 与 Manifest 的 ready barrier。probe/NFO/thumbnail worker 只能在 barrier 就绪后启动。
- v3 以数据库中的未就绪 Manifest 作为增量扫描准入屏障。全量 postprocessing target 物化和可消费前，增量扫描不能改写相同 filesystem generation；旧 workflow/discovery format 的任务默认视为 targets 已就绪。
- 只有完整发现且当前可用的根路径可以生成缺失 delta；删除前保留第二次文件状态确认。不可用、不完整、取消或 I/O 失败都不能触发该根路径的批量删除。
- 新版只为 destructive REMOVE 持久化 delta；文件系统索引、媒体索引、REMOVE 状态和进度在有界短事务中原子提交。postprocessing targets 在索引完成后独立分批生成，不重置冲突已存在的 target 状态。
- 普通列表可以读取已提交的安全正向批次；首页仍保留旧稳定快照，直至全部可用根路径完成索引及缺失确认。已提交的正向索引不因后续 root unavailable 或取消而回滚，但该 root 永不据此执行删除。
- SQLite 和 PostgreSQL 共用相同的存储状态机及核心 SQL 能力，不依赖 PostgreSQL 专属 COPY、`UPDATE ... FROM`、临时表或跨库扫描长事务。
- `scan_manifest_entries` 的主键已覆盖按 manifest/root/path/sequence 查找；不得再创建同列序的重复辅助索引，以免每条 observation 重复维护 B-tree。Migration 0134 同步移除该冗余索引。
- 新 discovery format 的文件存在性由 ADR-044 的紧凑 presence ledger 表示；本 ADR 中逐文件完整 observation 的合同适用于旧 discovery format 1/2。
- `ScanCompleted` webhook 和 `JOB_COMPLETED` 继续表示索引完成；现有 `POSTPROCESSING` 阶段表示后台后处理仍在运行，完成后转为 `IDLE`。不增加公开 webhook、API 状态或 Emby 合同。
- 成功全量索引结束后，先重建并原子替换共享及已有用户首页快照，再发布无业务载荷的 `home` 事件。失败/取消只公布已提交的安全状态；后处理的局部元数据更新继续按现有机制触发首页失效。
- 升级迁移只新增结构，不访问文件系统、不回填全库、不把旧队列伪装成 Manifest。启动时将没有 Manifest 的旧版活动全量任务标记为 `CANCELLED` 并保留诊断信息；管理员重试创建新 Manifest。新 Manifest 的可重试失败/取消任务保留 checkpoint；已完成 Manifest 的路径内容按有界批次清除，只保留不含路径的摘要。

## 考虑过的方案

### 继续扩展 `reconciliation_scan_entries`

该表是可复用的持久工作队列，但其 status 表示工作是否已消费，不能同时承担观察不可变性、根覆盖状态、差异基线和 delta 应用状态。将这些语义全部塞进一个队列表会让重试、清理和并发调和难以区分，因此保留该表用于旧任务兼容及历史清理，新扫描使用单独模型。

### 以后处理完成作为扫描完成

当前 `ScanCompleted` 和 `JOB_COMPLETED` 已在任务进入 `POSTPROCESSING` 后发布；把它们延迟到缩略图等全部完成会改变可观察时点，并继续阻塞首页刷新，因此保持索引完成语义。

### PostgreSQL 专属批量导入路径

这会令 SQLite 和 PostgreSQL 的事务/错误处理分叉。先使用两种后端都支持的有界批次，只有跨后端基准明确证明必要时，另行评估可选优化。

### 每个文件一个正向 delta 的两阶段 apply

把 60,000 个 ADD 分别写成 delta，再在后续阶段重新加载、校验、索引并更新每条 delta 状态，会重复存储路径/基线并增加完整扫描的数据库往返。新版在发现事务中完成安全的正向写入，以该事务和目录 frontier 作为恢复边界；只有需要删除既有条目的 REMOVE 候选仍进入 delta 表。该方案减少正向 delta 的持久写入，但保留不可变 observations、文件二次校验、CAS、目录恢复点和完整根删除门槛。

## 后果

- Manifest 新增 schema、storage 类型、状态转换、恢复与分批清理逻辑，必须有 SQLite 与 PostgreSQL 的等价迁移和测试。
- 新版新文件可随有界 discovery 批次提交；普通目录列表可看到已提交的正向结果，首页则在 Manifest 完整索引点稳定切换。用户不用等待 NFO、probe 或缩略图完成。
- 升级只增加 workflow version 和每 root 序号分配字段；已有活动 Manifest 仍由旧执行器恢复，不在升级时重算、删除或伪造其 delta 状态。
- 没有关联 Manifest 的旧版活动全量任务不会被不完整观察数据错误转换；仍按现有启动恢复规则安全取消并要求管理员重新发起扫描。有关联旧 workflow version 的 Manifest 则由对应旧执行器继续恢复。
- 本机 SQLite/ARM 性能不能证明 PostgreSQL、x86 NAS 或生产媒体盘性能，性能记录必须标明实际后端和硬件。

## 验收依据

阶段 21 的 LUX-265 至 LUX-270 覆盖 schema/storage、发现、delta/CAS、首页与事件、升级/清理以及 SQLite/PostgreSQL 性能和兼容性阶段门。
