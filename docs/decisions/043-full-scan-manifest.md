# ADR-043：全量扫描使用持久化 Manifest

## 状态

已接受

## 日期

2026-09-24

## 背景

全量扫描已经使用持久化目录/文件工作队列、文件指纹、短批次事务和缺失文件二次确认。工作队列只描述待做工作，不构成不可变的文件系统观察快照，也不能将扫描发现的差异与后来完成的增量扫描安全区分。首页快照又在完整后处理后才切换，导致“索引完成”和“后处理完成”的任务通知时点不清晰。

LUX-154、LUX-187、LUX-230 对目录发现、首页可见时点和快照刷新时机存在语义分歧。本 ADR 将它们统一为：扫描批次提交期间首页保留旧稳定快照；Manifest 索引和缺失确认完成后切换首页快照；probe、NFO、图片、封面和缩略图继续异步后处理。

## 决策

- 全量扫描拥有独立的持久化 Manifest 模型，包含 Manifest 生命周期、逐根路径覆盖状态、持久目录 frontier、不可变文件系统 observations 和待应用 deltas。
- `filesystem_entries` 继续作为当前文件系统索引事实来源。Manifest 通过 `library_root_id + relative_path` 与它比较，不从 `media_items` 单独推导删除。
- Manifest observation 不原地覆盖。扫描应用时对新增/变化文件再次 stat/fingerprint；对索引基线 ID 和 fingerprint 做 CAS，避免全量扫描覆盖并发增量结果。
- 只有完整发现且当前可用的根路径可以生成缺失 delta；删除前保留第二次文件状态确认。不可用、不完整、取消或 I/O 失败都不能触发该根路径的批量删除。
- 文件系统索引、媒体索引、后处理 targets、delta 状态和任务进度在有界短事务中原子提交。
- SQLite 和 PostgreSQL 共用相同的存储状态机及核心 SQL 能力，不依赖 PostgreSQL 专属 COPY、`UPDATE ... FROM`、临时表或跨库扫描长事务。
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

## 后果

- Manifest 新增 schema、storage 类型、状态转换、恢复与分批清理逻辑，必须有 SQLite 与 PostgreSQL 的等价迁移和测试。
- 文件索引可以批量提交，首页则在 Manifest 完整索引点稳定切换；用户不用等待 NFO、probe 或缩略图完成。
- 旧版活动全量任务不会被不完整观察数据错误转换或继续跑旧扫描代码；管理员需要重新发起一次 Manifest 全量扫描。
- 本机 SQLite/ARM 性能不能证明 PostgreSQL、x86 NAS 或生产媒体盘性能，性能记录必须标明实际后端和硬件。

## 验收依据

阶段 21 的 LUX-265 至 LUX-270 覆盖 schema/storage、发现、delta/CAS、首页与事件、升级/清理以及 SQLite/PostgreSQL 性能和兼容性阶段门。
