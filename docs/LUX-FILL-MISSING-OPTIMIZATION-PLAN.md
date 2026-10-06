# FILL_MISSING 创建频率与重复补全优化方案

## 当前实现状态

以下修复已经落地并部署到 FNOS `192.168.10.50`：

- 只有本轮新确认的 missing claim 才会触发自动 `FILL_MISSING`。
- 近期 `DEFERRED` job 参与去重，避免 provider 暂不可用时立即重复建任务。
- completeness fingerprint 只保留会改变补全计划的输入，普通 overview、评分等变化不会重置整套能力。
- completeness 失败记录在 `retry_after` 到期前不会重新领取。
- 同一媒体库的自动补全优先并入已有 `QUEUED` job，单个 job 仍限制为 100 条。
- 默认扫描并发为 2，全量 `RECONCILE_LIBRARY` 使用全局串行队列。
- 自动 `FILL_MISSING` worker 默认并发限制为 2，避免多个 job 各自启动 8 个 worker 放大 CPU。
- person manifest 与 item people relation 写入按 key 分片串行，避免同一文件锁竞争导致反复重试。

对应提交：`cea0c50d`、`8389bc0e`、`a2272b2b`、`086c8769`、`efc5e323`、`e9185dcb`、`48d6357f`。

FNOS 当前运行 revision：`9979c4ba`；schema version：`160`。部署后 outbox 已清空，最近采样 Lux/PostgreSQL CPU 约为 2.9%/2.4%。

日期：2026-10-05

状态：LUX-401 的 job item 请求快照部分已在隔离 worktree 本地实现并完成 SQLite 定向验证；未部署。退避策略、剩余调度策略和 FNOS 指标仍按下文计划处理。

## 目标

降低 `FILL_MISSING` 的创建频率，避免同一资源在输入没有实质变化时反复补全，同时保留以下行为：

- 新增媒体或真实缺失能力仍可自动补全；
- provider 暂时不可用时可以重试，但有明确退避；
- 人工 `FULL_REFRESH`、重新匹配和配置变化不被自动去重误伤；
- SQLite/PostgreSQL 的事务、并发和现有任务状态合同保持一致；
- 全量扫描一次只运行一个库，增量扫描仍可及时处理。

## 已确认的生产证据

- FNOS 当前生产镜像 revision 为 `f52f7fe4251bee595bfda0cc8475bf3`，尚未包含最新的重复补全修复。
- 一次全量扫描后，多个库进入 `POSTPROCESSING`，并持续产生 `FILL_MISSING`。
- 10 分钟内曾创建约 238 个 `FILL_MISSING` 任务。
- 同一 episode 在 10 分钟内进入 5 个不同的 `FILL_MISSING` 任务。
- 该 episode 的 NFO hash 连续采样保持不变，因此这次重复创建已经不能归因于 NFO 文件反复写入。
- FNOS 上大量任务为 `DEFERRED`，而旧生产版本的去重查询只排除 `QUEUED/RUNNING`。
- 当前 completeness 数据中，约 8 万条能力记录为 `READY + is_missing=1`。这表示本地检查确认能力缺失，不表示在线补全已经成功。
- PostgreSQL 没有锁等待；高 CPU 主要来自全量后处理、元数据补全和重复任务调度。

## 已确认的问题

### P0：旧扫描路径会重复提交已有 missing 状态

扫描器会把所有“当前可请求”的 item 放入 `eligible_fill_missing_item_ids`，即使本轮没有新的 completeness claim。存储层随后根据旧的 `READY + is_missing=1` 再创建任务。

已有修复：只有本轮新 claim 且 `is_missing=true` 的 item 才进入自动入队列表。该修复已在共享 checkout 的 `cea0c50d` 中，但 FNOS 尚未部署。

验收：相同 item、相同 fingerprint、没有新的 completeness claim 时，重复扫描不创建新 job。

### P0：`DEFERRED` 没有参与近期去重

旧逻辑只检查 `QUEUED/RUNNING`。provider 不可用后，任务变为 `DEFERRED`，下一次扫描仍会再次创建相同 item 的任务。

已有修复：近期 `DEFERRED` 纳入去重窗口。该修复在 `8389bc0e` 中，当前生产未部署。

验收：同一 item 在 deferred 冷却窗口内不重复入队；人工 retry 仍可立即执行。

### P1：completeness fingerprint 输入过宽

当前 fingerprint 包含标题、简介、评分、NFO 缓存和其他不会决定某个缺失能力是否可请求的字段。任意普通元数据变化都可能使所有能力的 fingerprint 变化，导致已经确认缺失的 poster、fanart、credits 等重新进入补全流程。

隔离工作树中已有候选修复 `510f24e2`：fingerprint 收窄为 item 类型、provider identity、scraper、锁定/来源状态、系列上下文、实际/可请求 capability plan 和 image policy；无关简介变化不再触发全能力重算。

该修复尚未合入共享 checkout，需先通过本方案的 capability 回归后再合入。

验收：

- 只改变 overview/rating 等无关字段，fingerprint 不变；
- provider ID、scraper、锁定状态、策略或缺失 capability 变化时 fingerprint 改变；
- XML 排列、NFO 缓存重建不改变 fingerprint。

### P1：`retry_after` 没有形成退避闭环

表中已有 `retry_after`，但 completeness 完成路径会清空它，provider 不可用的 `DEFERRED` 任务也没有把下次自动尝试时间写回 completeness。结果是系统只能依赖任务状态查询或下一次扫描，缺少按能力和 provider 的退避控制。

拟修复：为 provider unavailable、低置信度和永久 unsupported 分开记录 retry policy；自动扫描只处理 `retry_after IS NULL OR retry_after <= now` 的记录。

验收：provider 不可用时按 5 分钟、30 分钟、6 小时等有界退避；同一失败期间不创建重复 job；人工重试绕过退避。

### P1：任务去重粒度只有 item，没有 capability/fingerprint 证据

`metadata_reidentify_job_items` 只保存 item 和状态。现在的 item 级去重无法区分：

- poster 缺失和 trailers 缺失是否是同一请求；
- 新 provider identity 是否替代了旧 provider identity；
- 当前 job 是否覆盖了新的 capability plan。

已实施（LUX-401，本地未部署）：双后端 job item migration 增加请求 fingerprint、规范化 capability JSON 与 claim 快照。相同快照沿用现有任务；queued item 更新为最新快照；running item 保存更新后的期望值，并在当前处理结束后最多重新排队一次；近期 `DEFERRED/SCRAPER_UNAVAILABLE` 只抑制相同快照。旧任务和手动创建的无快照 item 保持兼容。当前验证是固定 SQLite 状态机，不证明 PostgreSQL/NAS 性能或 FNOS CPU 收益。

验收：旧 job 不会阻止新 capability；相同输入不会产生第二个有效 job；并发扫描和并发手动刷新只保留一个有效请求。

### P2：每个批次都可能新建 job，缺少 queued job 合并

当前每批最多 100 个 item，但不同 local metadata batch 会分别创建 job。大量小批次会产生许多小 job，即使它们属于同一 library、同一策略和相近时间窗口。

拟修复：同一 library 的自动 `FILL_MISSING` 在短窗口内优先追加到一个 `QUEUED` job；只在 job 已 claim、输入 fingerprint 不兼容或达到上限时创建新 job。每个 job 仍保持 item 上限和分页上限。

验收：同一批扫描产生的 50 个小请求合并为有界数量的 job；运行中 job 不被修改，避免 worker 看到不一致输入。

### P2：全量扫描策略和实时补全策略边界不够清晰

`complete_local_metadata_completeness_for_item_ids` 对全量/backfill 使用 library 的 `scan_missing_metadata_auto_match_enabled`，对 incremental 使用 scan job 的快照。这个边界必须继续保持明确：

- 全量扫描是否允许在线补全必须由库策略显式决定；
- incremental 必须只使用创建 job 时保存的策略快照；
- workflow 3 不得在扫描末尾再次创建整库 FILL_MISSING。

拟修复：给调用入口显式标注 `FullScanPolicy`、`IncrementalPolicy`、`ManualPolicy`，禁止通过 `Option<bool>` 隐式推断。

## 计划阶段

### 阶段 1：调度正确性

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

### 阶段 2：任务幂等和合并

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

### 阶段 3：扫描队列和资源保护

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

已有候选提交：`534fbaa3`。

### 阶段 4：可观测性和上线

记录以下指标：

- 每分钟新建的 FILL_MISSING job 数；
- 每个 item/capability/fingerprint 的去重命中数；
- deferred 原因、retry_after、下一次 due 时间；
- 每个 library 的扫描队列等待时间；
- FILL_MISSING item 成功、缺失仍在、provider unavailable 数；
- PostgreSQL CPU、WAL、活动查询和锁等待。

上线顺序：先部署代码到测试环境，验证 job 创建率下降；再在 FNOS 降到并发 2、串行全量扫描；最后观察至少一个完整扫描周期。

## 不在本次方案内

- 不删除历史任务；
- 不直接把 `READY + is_missing=1` 改成 complete；
- 不关闭所有自动元数据补全作为永久方案；
- 不通过提高 PostgreSQL 连接数或 worker 并发掩盖问题；
- 不在用户请求路径执行全库扫描或在线刮削。

## 完成验收

- 同一 item/capability/fingerprint 在重复扫描中不产生新 job；
- provider unavailable 进入有界退避，不在每次扫描重试；
- 新 capability、provider/config 变化和人工刷新仍能创建请求；
- 一次只执行一个全量库扫描，增量任务可抢占或优先等待；
- SQLite/PostgreSQL 定向测试、全量 Rust 门禁和 FNOS 只读指标均通过。
