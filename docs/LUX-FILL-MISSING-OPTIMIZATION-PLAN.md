# FILL_MISSING 创建频率与重复补全优化方案

## 当前实现状态

截至 2026-10-07，LUX-401 至 LUX-406 的重复补全、扫描调度和扫描后本地 metadata 读取优化已在本地分支实现，并有 SQLite/PostgreSQL 合同或定向回归覆盖。当前 worktree 的修复尚未部署到 FNOS；本地 ARM 测试不能证明服务器 CPU 已下降。

- 自动 `FILL_MISSING` 仅由本轮新确认的 missing claim 触发；稳定 fingerprint 与请求快照用于区分真正变化的补全意图。
- 活动任务和带快照的近期 provider 失败参与去重；自动 provider 失败在 5 分钟、30 分钟、最高 6 小时的退避期内不重复创建请求。
- completeness fingerprint 只包含会改变补全计划的输入；本地 completeness 失败在 `retry_after` 到期前不会重新领取。
- 入队会复用按创建顺序找到的 queued job 容量，单个 job 最多 100 项。
- 默认扫描并发为 2，全量 `RECONCILE_LIBRARY` 使用容量为 1 的共享队列；自动 `FILL_MISSING` worker 并发限制为 2。
- 全量扫描、增量扫描、后台回填和精确目录本地刷新使用显式的补全策略来源。
- 扫描图片阶段的 source 快照传给 NFO 阶段；NFO 处理只做一次有界 source 身份确认，按批次读取有 NFO 的条目元数据，无 NFO 条目跳过 metadata 读取。
- 之前针对 NFO 等价写入、内部写 watcher 反馈、metadata no-op 写入及任务活动查询的本地优化保留在当前代码树中。

本地改动包括请求快照/稳定 fingerprint、provider 退避、queued 容量复用、具名策略和跨库串行队列回归。生产版本和 CPU 只有在再次读取 FNOS 当前镜像、revision、schema 与运行指标后才能确认。

历史记录：2026-10-05 的采样曾记录 Git revision `9979c4ba`、schema version `160`、outbox 已清空和 Lux/PostgreSQL CPU 约为 2.9%/2.4%；另一个历史条目记录镜像 revision `f52f7fe4251bee595bfda0cc8475bf3`。这些是不同时间点的存档，当前映射和运行状态尚未重新核实。

## 目标

降低 `FILL_MISSING` 的创建频率，避免同一资源在输入没有实质变化时反复补全，同时保留以下行为：

- 新增媒体或真实缺失能力仍可自动补全；
- provider 暂时不可用时可以重试，但有明确退避；
- 人工 `FULL_REFRESH`、重新匹配和配置变化不被自动去重误伤；
- SQLite/PostgreSQL 的事务、并发和现有任务状态合同保持一致；
- 全量扫描一次只运行一个库，增量扫描仍可及时处理。

## 历史生产采样（不是当前状态）

- 一次历史采样记录 FNOS 镜像 revision 为 `f52f7fe4251bee595bfda0cc8475bf3`；该标识与其他采样记录的 Git revision 不同，当前映射未核实。
- 一次全量扫描后，多个库进入 `POSTPROCESSING`，并持续产生 `FILL_MISSING`。
- 10 分钟内曾创建约 238 个 `FILL_MISSING` 任务。
- 同一 episode 在 10 分钟内进入 5 个不同的 `FILL_MISSING` 任务。
- 该 episode 的 NFO hash 连续采样保持不变，因此这次重复创建已经不能归因于 NFO 文件反复写入。
- FNOS 上大量任务为 `DEFERRED`，而旧生产版本的去重查询只排除 `QUEUED/RUNNING`。
- 当前 completeness 数据中，约 8 万条能力记录为 `READY + is_missing=1`。这表示本地检查确认能力缺失，不表示在线补全已经成功。
- 当时 PostgreSQL 没有锁等待；采样判断高 CPU 主要来自全量后处理、元数据补全和重复任务调度。

## 已确认的问题

### P0：旧扫描路径会重复提交已有 missing 状态

扫描器会把所有“当前可请求”的 item 放入 `eligible_fill_missing_item_ids`，即使本轮没有新的 completeness claim。存储层随后根据旧的 `READY + is_missing=1` 再创建任务。

已实现：只有本轮新 claim 且 `is_missing=true` 的 item 才进入自动入队列表。相同 fingerprint/capability 的重复扫描由请求快照和存储层去重覆盖。2026-10-07 现场复核显示 FNOS 仍运行 revision `8412cc2a`、schema 164，尚未包含本地 0165 优化分支。

验收：相同 item、相同 fingerprint、没有新的 completeness claim 时，重复扫描不创建新 job。

### P0：`DEFERRED` 没有参与近期去重

旧逻辑只检查 `QUEUED/RUNNING`。provider 不可用后，任务变为 `DEFERRED`，下一次扫描仍会再次创建相同 item 的任务。

已实现：近期带相同请求快照的 `DEFERRED` provider 失败参与去重；LUX-402 再增加逐 item 冷却时间。当前 FNOS 为 revision `8412cc2a`、schema 164，未部署本地优化分支。

验收：同一 item 在 deferred 冷却窗口内不重复入队；人工 retry 仍可立即执行。

### P1：completeness fingerprint 输入过宽

当前 fingerprint 包含标题、简介、评分、NFO 缓存和其他不会决定某个缺失能力是否可请求的字段。任意普通元数据变化都可能使所有能力的 fingerprint 变化，导致已经确认缺失的 poster、fanart、credits 等重新进入补全流程。

已实现并保留在当前分支：fingerprint 收窄为 item 类型、provider identity、scraper、锁定/来源状态、系列上下文、实际/可请求 capability plan 和 image policy；无关简介变化不再触发全能力重算。相关 capability 状态机回归通过。

验收：

- 只改变 overview/rating 等无关字段，fingerprint 不变；
- provider ID、scraper、锁定状态、策略或缺失 capability 变化时 fingerprint 改变；
- XML 排列、NFO 缓存重建不改变 fingerprint。

### P1：provider 不可用的自动 job 缺少逐 item 退避

`item_metadata_completeness.retry_after` 只控制本地 completeness 检查失败后的重新领取，不代表在线 provider 的退避。自动 `FILL_MISSING` 的 provider 不可用结果保存在 `metadata_reidentify_job_items.error='SCRAPER_UNAVAILABLE'`，当前仅通过 `DEFERRED` job 的一小时窗口去重；持续不可用时，后续扫描会再次创建相同请求。能力级 scraper attempt 也不能替代 job 级退避，因为尚未获得 provider identity 或 scraper 服务整体不可用时没有可用的能力 attempt key。

已实现（LUX-402，本地未部署）：双后端 job item migration 增加自动失败次数、截止时间和一次性消费标记。同一 fingerprint/capability 在 5 分钟、30 分钟、最多 6 小时的退避期内合并/去重；到期后由下一次自动扫描重试，并将次数延续到新 job。派发重试时在同一事务中批量消费旧到期失败记录，防止重试完成后由历史记录再次解锁；当新的 fingerprint/capability 取代旧快照时，也在新请求提交时消费旧 provider 失败，防止旧快照反复唤醒新请求。手动 retry 和新请求快照会清除消费标记。输入快照变化不继承旧退避；无快照人工 job 不改变原有一小时去重语义。迁移只为已有自动 deferred provider 失败且带请求快照的 item 安排首次 5 分钟冷却。一次有界 SQL 读取同时检查活动任务与匹配的历史 provider 失败；只有到期重试派发或 superseded failure 被发现时才执行有界批量更新。SQLite 状态机、迁移和 PostgreSQL storage contract 覆盖成功重试、手动 retry 与旧快照替代；build/fmt/Clippy 和定向测试已验证。全目标结果及两个在干净基线复现的集成失败详见 `docs/LUX-DEVELOPMENT.md` 的 LUX-402 结果。未部署 FNOS，不能据此宣称生产 CPU 降幅。

验收：持续 provider 不可用时同一输入按有界退避重试且不在冷却期重复创建 job；新 capability、fingerprint 和人工 retry 不被旧请求退避误伤。

### P1：任务去重粒度只有 item，没有 capability/fingerprint 证据

`metadata_reidentify_job_items` 只保存 item 和状态。现在的 item 级去重无法区分：

- poster 缺失和 trailers 缺失是否是同一请求；
- 新 provider identity 是否替代了旧 provider identity；
- 当前 job 是否覆盖了新的 capability plan。

已实施（LUX-401，本地未部署）：双后端 job item migration 增加请求 fingerprint、规范化 capability JSON 与 claim 快照。相同快照沿用现有任务；queued item 更新为最新快照；running item 保存更新后的期望值，并在当前处理结束后最多重新排队一次；近期 `DEFERRED/SCRAPER_UNAVAILABLE` 只抑制相同快照。旧任务和手动创建的无快照 item 保持兼容。SQLite 状态机及 PostgreSQL 16 集成合同通过；不代表 FNOS CPU 或 NAS 性能收益。

验收：旧 job 不会阻止新 capability；相同输入不会产生第二个有效 job；并发扫描和并发手动刷新只保留一个有效请求。

### P2：多条 QUEUED job 时追加逻辑只检查最旧任务

当前入口会把新 item 合并到该库最旧的一条 `QUEUED FILL_MISSING` job，最多 100 项；如果这条任务已满，即使同库还有较新的未满 `QUEUED` job，入口仍会新建 job，而不会继续填充其他可用队列容量。通常扫描请求会复用同一条队列，但已有多条队列时会形成小 job。

已实现（LUX-403）：按创建顺序遍历有剩余容量的 queued job，并有界分配；不修改 `RUNNING` job，所有 job 仍不超过 100 项。SQLite 回归和 PostgreSQL progressive-scan storage contract 均验证容量复用。

验收：同一媒体库存在多条未满 queued job 时，新 item 按序填满已有容量，再创建新 job；每个 item 只出现一次，且 job/item 计数一致。

### P2：全量扫描策略和实时补全策略边界不够清晰

`complete_local_metadata_completeness_for_item_ids` 对全量/backfill 使用 library 的 `scan_missing_metadata_auto_match_enabled`，对 incremental 使用 scan job 的快照。这个边界必须继续保持明确：

- 全量扫描是否允许在线补全必须由库策略显式决定；
- incremental 必须只使用创建 job 时保存的策略快照；
- workflow 3 不得在扫描末尾再次创建整库 FILL_MISSING。

已实现（LUX-404）：调用入口使用具名 trigger 和解析后的策略类型，覆盖全量扫描、增量扫描、后台回填与精确目录本地刷新，禁用时不探测刮削器；策略和存储合同回归通过。

### P2：扫描图片与 NFO 阶段重复读取 source 和 metadata

扫描本地 metadata outbox、库级补回和精确目录刷新会先查询 preferred source 并处理图片，随后 NFO 阶段再次按 entry ID 读取完整 source 行；每个实际存在 NFO 的 item 还会逐条读取完整 metadata 行。没有 NFO 的条目不需要 metadata 行。

已实现（LUX-406）：图片阶段把已读取 source 快照传给 NFO 阶段；开始处理前用有界身份查询确认原 preferred source 仍有效，处理完成前保留原有 freshness 复核。NFO/层级路径按批次发现，metadata 只对实际存在 NFO 的 item 执行一次有界批量读取。无 NFO 批次不读 metadata；source snapshot 分组避免深拷贝无关字段；家庭视频路径检查错误继续作为条目失败上报。该优化影响本地 NFO 后处理，不创建或触发在线 `FILL_MISSING`。

SQLite 回归确认两个 NFO 共用一条 metadata 批量读取、无 NFO 跳过 metadata 查询、陈旧 preferred source 被排除，并确认家庭视频 `try_exists` I/O 错误未被误判成 NFO 缺失。`scanned_metadata` 和 `scanning_jobs` 集成目标通过；不据此推断 FNOS CPU 或 NAS 收益。

## 实施与验证状态（2026-10-07）

### 阶段 1：调度正确性（本地完成）

文件范围：

- `src/application/scanner.rs`
- `src/application/candidates.rs`
- `src/storage/metadata.rs`
- `src/storage/repository_tests.rs`

内容：

1. 合入“只有新 claim 才自动入队”；
2. 合入 capability 相关稳定 fingerprint；
3. 为 deferred/provider unavailable 增加 retry_after 计算和 due 查询；
4. 明确人工刷新绕过自动去重。

验证：scanner、candidate、repository metadata 定向测试；同一 item 重复扫描、provider unavailable、fingerprint 变化、人工 refresh 四组回归。

### 阶段 2：任务幂等和合并（本地完成）

文件范围：

- `migrations/`
- `migrations-postgres/`
- `src/storage/metadata.rs`
- `src/storage/jobs.rs`
- `src/application/reidentify.rs`

内容：

1. 增加 capability mask/input fingerprint 的持久边界；
2. 原子化“检查并入队”，为变化中的 queued/running intent 保留最新请求；
3. queued job 合并并按签名更新/去重；
4. 保留 SQLite/PostgreSQL 从空库迁移和并发写测试。

验证：双后端 migration、并发入队测试、SQL 次数和 job 数量基准。

### 阶段 3：扫描队列和资源保护（本地完成）

文件范围：

- `src/application/scanner.rs`
- `src/config/mod.rs`
- `compose.yaml`
- `README.md`
- `docs/LUX-DEVELOPMENT.md`

内容：

1. 默认扫描并发 2；
2. 全量库扫描全局串行，其他库保持 pending；
3. 增量扫描拥有优先权；
4. 保留动态 CPU/IO 降档和管理员手动 override。

当前代码保留管理员并发覆盖，并依据容器 CPU、内存和存储延迟自动降级后台 worker；并发保护不只依赖固定默认值。

### 阶段 4：生产观测与部署验证（待运行现场复核）

记录以下指标：

- 每分钟新建的 FILL_MISSING job 数；
- 每个 item/capability/fingerprint 的去重命中数；
- deferred 原因、retry_after、下一次 due 时间；
- 每个 library 的扫描队列等待时间；
- FILL_MISSING item 成功、缺失仍在、provider unavailable 数；
- PostgreSQL CPU、WAL、活动查询和锁等待。

以上是部署后的观测清单，不代表当前分支已经部署或生产验收完成。代码侧的退避、队列容量、补缺策略和扫描串行约束已完成；要证明 FNOS 收益，还需核对实际运行 revision/schema，观察完整扫描周期，并对比 job 创建率、CPU 与队列等待时间。当前任务未部署代码。

预部署现场基线（2026-10-07 13:33–13:40，FNOS `192.168.10.50`，只读采样）：运行镜像 `pdzhou/lux:test` 标记 revision `8412cc2a`，`/health/ready` 报告版本 `0.5.19`、schema 164；本地优化分支及 migration 0165 尚未部署。容器环境 `LUX_SCAN_CONCURRENCY=2`；8 个库保存值为 2、1 个库保存值为 32，但全局环境覆盖优先于库设置。采样窗口没有 PENDING/RUNNING 扫描 job，也没有最近 10 分钟新建的扫描或 metadata job；有一个旧 `FILL_MISSING` job 仍在运行，5,042 项中 processed_count 从 4,164 增至 4,361，13:39 时项目状态为 COMPLETED 4,123、FAILED 238、PENDING 680、RUNNING 1。Docker 单核口径 CPU 快照中 Lux 为 6.53%–44.52%、PostgreSQL 为 9.56%–238.59%；主机有 12 个逻辑核，13:39 单次 `top` 报告 89.7% idle。`pg_stat_activity` 快照未见 Lock 等待；`pg_stat_statements` 未安装，PostgreSQL statement logging 关闭，因此不能从该采样归因到具体 SQL。该数据是部署前短时基线，不代表稳定均值或改动后的 CPU 收益。

后续只读采样（2026-10-07 15:20，FNOS `192.168.10.50`）：容器仍运行 revision `8412cc2a`、schema 164；该时刻 Docker 单次 CPU 快照 Lux 1.15%、PostgreSQL 4.63%，`FILL_MISSING` 没有 QUEUED/RUNNING job，最近 30 分钟没有新建 metadata job。历史记录仍有 14,381 个未请求取消的 DEFERRED `FILL_MISSING` job，包含 335,965 条 `SCRAPER_UNAVAILABLE` 失败 item、7,566 个不同 item；这些失败行均没有 LUX-401 请求快照，因此不会由 0165 的自动退避兼容路径批量唤醒。对当前完整度仍标记缺失、媒体库自动匹配开启且带请求快照的失败项计数为 0。该 CPU 值只是空闲时的单点快照，不能证明负载趋势或本地修复效果；分支仍未部署。

现场复查（2026-10-07 15:29–15:31，FNOS `192.168.10.50`）：`/health/ready` 正常，仍为 revision `8412cc2a`、schema 164；扫描并发环境变量仍为 2。Docker CPU 单核口径从 Lux/PostgreSQL 2.73%/37.45% 波动到 7.56%/78.44%，约 7 秒后降到 1.01%/0.35%，再过 8 秒为 4.36%/2.01%，表明是短时尖峰而非持续高位。同期数据库快照未见排队/运行 metadata item，过去 10 分钟未见新建 metadata job；16 个 Lux PostgreSQL client connection 均为空闲，采样时只有诊断查询自身处于 active，未见锁等待。历史未取消 DEFERRED `FILL_MISSING` 为 14,389 个 job、335,965 个 `SCRAPER_UNAVAILABLE` item；`pg_stat_statements` 未安装，无法归因尖峰 SQL。该采样不能证明本地分支优化收益，生产仍运行旧 revision。

## 不在本次方案内

- 不删除历史任务；
- 不直接把 `READY + is_missing=1` 改成 complete；
- 不关闭所有自动元数据补全作为永久方案；
- 不通过提高 PostgreSQL 连接数或 worker 并发掩盖问题；
- 不在用户请求路径执行全库扫描或在线刮削。

## 完成验收

- [x] 同一 item/capability/fingerprint 在重复扫描中不产生新 job。
- [x] provider unavailable 进入有界退避，不在每次扫描重试。
- [x] 新 capability、provider/config 变化和人工刷新仍能创建请求。
- [x] 一次只执行一个全量库扫描；增量任务优先行为的既有回归继续通过。
- [x] SQLite/PostgreSQL 定向合同覆盖通过，Rust build/fmt/Clippy 通过；全目标测试的基线失败单独记录在开发任务结果中。
- [ ] FNOS 部署 revision 与只读运行指标复核；本地修改不能替代生产 CPU 验证。
