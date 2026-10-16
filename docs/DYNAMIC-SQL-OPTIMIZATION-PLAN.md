# 动态 SQL 构建与字符串分配优化设计

日期：2026-10-16。状态：分步实施中；LUX-562 仅建立局部基线，尚未实施生产优化或确认性能收益。

基线：当前 `origin/test` 的 `e7107bfe`（LUX-562 开始时）。
设计分支：`codex/dynamic-sql-optimization`。
范围：用户提供的 `cpu-hotspot-analysis.md` 第五点。

## 1. 决策

采用 **C 的减少复制思路 + 改良后的 A**：先消除查询入口的无效复制，再复用完整 SQL 模板，最后只在语义安全的读取中减少 SQL 形状。
**B 临时表不进入首轮实现**；仅在大集合被反复使用、A+C 后仍有明确瓶颈时单独实验。

这里的 C 不等于将所有 `String` 改成 `Cow<str>`。SQL 文本优先使用当前 SQLx 0.9.0 已提供的 `SqlStr`；业务字符串优先借用，只有混合借用和拥有数据的局部集合才使用 `Cow`。

| 方案 | 解决的问题 | 成本与限制 | 本设计的选择 |
| --- | --- | --- | --- |
| A：固定批次、复用 SQL | 重复构造 SQL；相同查询出现过多占位符数量 | 现有代码已经分批，单改批次大小收益有限；不能用假行补齐写入 | 主线：完整 SQL 模板复用；安全读取分桶；写入保留精确行数 |
| B：临时表 + JOIN | 同一大集合在多个查询中重复绑定、构造和处理 | 装载/清理、连接固定、异常恢复和两种数据库的差异；单次查询未必划算 | 保留实验，首轮不落地 |
| C：减少字符串复制 | Rust 侧 SQL、路径、ID 和整个文件记录的重复分配 | 无法单独减少 SQL 形状；SQLx Any 绑定字符串仍需拥有数据 | 第一优先级，与 A 配合 |

设计已获用户授权进入实施。S0 已登记为 LUX-562；其余工作包仍须按实施时可用编号分别登记为独立 `LUX-*` 任务并补入 `LUX-DEVELOPMENT.md`，一次只实施一个任务。工作包编号 S0–S7 不是项目任务编号。

## 2. 已核实的现状

### 2.1 SQLx 已经缓存预编译语句

`Cargo.lock` 锁定 SQLx 0.9.0。`Database::query/query_as/query_scalar` 使用 SQLx 查询对象，其默认 `persistent = true`；驱动按连接缓存相同文本的预编译语句。SQLite/PostgreSQL 驱动的默认容量均为 100 条，当前连接初始化没有覆盖它。

因此，原报告中“SQLite 每次都重新解析和编译”的说法过于绝对：相同文本在同一连接命中缓存时可以重用；首次执行、换连接或缓存淘汰后仍需要准备。占位符数量不同会形成不同文本，但不代表每次都未命中。

必须区分两层：本设计缓存的是 **SQL 文本**；SQLx 缓存的是 **某个连接上的 prepared statement**。两者互补，不能把连接绑定的 statement/Query 对象全局共享。

### 2.2 查询入口存在确定的复制

位置：`src/storage/repository.rs` 的 `adapt_sql_for_backend()` 和三个查询入口。

- SQLite 分支对 `SqlStr::as_str()` 无条件 `to_owned()`：静态 SQL 和已经拥有的动态 SQL 都被重新复制。
- PostgreSQL 分支每次重新扫描 SQL，且每个占位符都调用 `parameter_index.to_string()`。
- SQLx `SqlStr` 可原样保留静态字符串、拥有的字符串和 `Arc<str>`；无需为 SQLite 再造字符串。
- SQLx `SqlStr` 从 owned 表示执行 `clone()` 时仍可能复制文本。因此模板应明确存为 `Arc<str>`，不能仅凭类型叫 `SqlStr` 就假定任意 clone 都便宜。

### 2.3 大部分热点已经分批

现有上限包括：通用扫描 DML 200 行、Manifest delta 100 行、路径查询 SQLite 2,900 / PostgreSQL 5,000 行，以及各类多列写入的独立上限。

对基线源码统计 `std::iter::repeat_n(` 的字面调用：storage 内 30 个文件共 215 处，`jobs/scan` 内 10 个文件共 27 处。这只是待筛选的位置清单，不是 CPU 占比证据，也不表示每处都是 IN 查询。

优先核对的扫描路径：

| 文件 | 现有工作 | 优化类别 |
| --- | --- | --- |
| `src/storage/filesystem.rs` | 分批按路径读取文件系统条目 | 完整模板、安全 IN 分桶 |
| `src/storage/jobs/scan/manifest/diff.rs` | 按路径读取 baseline | 完整模板、安全 IN 分桶 |
| `src/storage/jobs/scan/manifest/discovery/paths.rs` | 新版 VALUES CTE 查已知路径；旧版 IN 查询 | 精确行数模板；按版本分开 |
| `src/storage/jobs/scan/manifest/discovery/presence.rs` | presence 更新、回读和 seen-path 写入 | 精确行数模板、局部借用 |
| `src/storage/jobs/scan/manifest/discovery/frontier.rs` | 目录插入、完成状态更新 | 精确行数模板 |
| `src/storage/jobs/scan/manifest/diff/insert.rs` | delta 重试校验及插入 | 精确行数模板 |
| `src/storage/jobs/scan/manifest/apply/finalize.rs` | CASE 批量更新 | 复用模板、去掉 `state_cases.clone()` |
| `src/storage/jobs/scan/manifest/indexes.rs`、`src/storage/media.rs` | 筛选已成功 claim 的电影/剧集后插入 | 去掉 `NewMovieFile/NewEpisodeFile` 深复制 |

`discovery/input.rs` 已大量使用借用，不需要再为了形式统一改为 Cow。暂不横向改造 catalog、people、sessions 等全部存储代码。

## 3. 方案细节

### 3.1 查询入口：直接转交 SqlStr

`adapt_sql_for_backend()` 改为返回 `sqlx::SqlStr`，三个查询入口直接交给 SQLx。

1. SQLite：`into_sql_str()` 后原样返回，静态/owned/Arc 表示均不复制文本。
2. PostgreSQL：没有 `?` 时原样返回；需要改写时沿用现有引号处理规则，用 `write!` 写入同一个预分配 String，避免每个编号的临时 String。
3. 动态 SQL 安全检查保留在模板构造边界；路径、ID、URL 等数据始终绑定，不插入 SQL 文本。
4. 不在本任务重写 SQL 词法分析器。已有引号行为保持兼容；如果实际受影响 SQL 出现注释、dollar quote 等未支持情形，先补复现并单独界定修复范围。

这一步不改查询结果、批次、事务、连接池或数据库结构，收益覆盖所有经过这三个入口的 SQLite 查询。

### 3.2 完整 SQL 模板：缓存最终文本

新增 storage 内部 `sql_templates.rs`，仅允许有限的查询种类。模板键包含：

`(查询种类, 数据库后端, SQL/工作流版本, 行数或容量档位)`。

后端已有不同语法的模板分别登记；必须记录固定参数数、每行参数数和参数绑定顺序，生成前校验总参数数。

- 缓存完整 SQL，而不是只缓存 `?, ?, ?`。PostgreSQL 模板保存已经编号的最终文本，命中时不再扫描替换。
- Database 内部提供仅接受模板枚举/形状的专用查询入口，校验后端后直接用共享文本创建 SQLx Query；保留现有测试查询计数。最终模板不再次经过 `adapt_sql_for_backend()`，避免把节省的工作放回通用入口。
- 当前满批模板使用懒初始化的 `Arc<str>`，对应 SQLx 的 Arced 表示。按需生成，不在启动时预热所有模板，不泄漏字符串。
- 构造时计算容量，直接向最终 String 追加静态片段、占位符和分隔符，消除 `collect::<Vec<_>>().join()` 的中间数组与片段字符串。
- 精确尾批先按需构造；只有基线证明相同行数反复出现时才启用小型尾批缓存。首轮不引入通用全 SQL 缓存。
- 需要尾批缓存时，键仍是有限枚举，不是任意 SQL 字符串。每个模板最多保留 4 个尾批，整个模板文本预算上限 8 MiB（含满批）；超限文本直接构造使用，不进入缓存。
- 缓存临界区内只查找/发布 Arc；构造在锁外，锁不跨 await。并发首次构造可以短暂重复，发布后共享同一结果。毒化/缓存失败退回构造路径，不使查询失败。
- 预算是缓存持有量上限；在途查询可能暂时持有已淘汰的 Arc，另行测量瞬时峰值。若固定模板全集已超预算，缩小允许缓存的查询种类。
- 先保持驱动默认 statement cache 和 persistent 策略，不用扩大连接缓存掩盖 SQL 形状问题。

### 3.3 只对安全的正向 IN 读取减少形状

首批仅接入 `list_filesystem_entries_for_paths()` 和 `list_scan_manifest_filesystem_baselines()`。

对每个现有 chunk，选择不小于真实元素数的最小容量：`1, 2, 4, ...`，最后追加该查询原有上限。例如上限为 500 时，容量集合是 `1..256` 的二次幂加 500；上限 2,900 / 5,000 同理。空输入直接返回。

示例：3 个路径使用 4 个 IN 占位符，最后一个绑定显式文本类型的 NULL。容量始终不超过原有上限，原有固定参数也计入参数预算。

适用条件必须同时满足：

- IN 是 WHERE 中直接的正向筛选条件，不能位于 NOT、`IS FALSE` 或返回布尔表达式中。
- 查询返回数据库匹配行，不以输入行的数量或顺序产生结果。
- 填充值使用 typed NULL；不使用假 ID、空路径或复制最后一个输入值。
- SQLite/PostgreSQL 测试证明空、重复、未命中和边界输入与原查询一致。

在这些条件下，NULL 改变未匹配项的三值逻辑，但不改变 WHERE 最终选中的行。这不是可推广到任意 SQL 的等价替换。

完整模板按容量复用，可将单个查询每后端的长度形状从最多 500 / 2,900 / 5,000 种，约束到最多 10 / 13 / 14 种。每批增加的 NULL 绑定少于真实输入数；绑定和传输成本必须纳入 A/B。

只有在变动尾批工作负载中降低了构造/准备成本且没有明显延迟回退，才保留分桶。若基线已有良好缓存命中而额外绑定反而更慢，保留精确行数模板，撤回该查询的分桶。这个选择不影响前两步成立。

### 3.4 写入、VALUES CTE 和计数敏感查询保留真实行数

以下内容只复用 SQL 文本，不补 NULL/重复行，不改变原 chunk 上限：

- `INSERT ... VALUES`、`ON CONFLICT` 和 `RETURNING`。
- `WITH incoming AS (VALUES ...)`，包括 paths/presence 的查询和更新。
- `CASE id WHEN ...` 批量更新。
- 重试基线校验、计数、序号预留、`rows_affected` 比较。

这是与第四点“SQLite 长写事务优化”的边界：第五点保持当前事务原子性、提交位置、计数和重试语义；不拆事务、不重设批次/并发、不增索引或 migration。

### 3.5 业务字符串：按所有权来源优化

按收益和改动范围排序：

1. `presence.rs` 的 `successfully_seen_paths`：完整更新成功时借用输入路径；需要 SQL 回读时拥有返回的 String，使用局部 `HashSet<Cow<'data, str>>`。两条路径的生命周期都限定在当前提交内。
2. `finalize.rs`：相同 CASE 片段复用引用或在最终模板内重复写入，不 clone 中间字符串。
3. `indexes.rs`：`file.clone()` 会复制整个文件记录，包含多个 String 和 fingerprint。改为收集 `&NewMovieFile` / `&NewEpisodeFile`，让 `media.rs` 的内部写入/预取 helper 消费引用；现有调用者通过一次引用列表适配，维持既有外层接口。
4. 必须拥有的结果继续拥有：数据库返回对象、以路径为 key 且 value 也拥有路径的 HashMap、异步队列持久数据、错误需要的路径等，不为了删 clone 改公共模型。

SQLx Any 的 `&str` 编码本身仍会构造拥有的参数；因此这一步只承诺减少应用预处理复制。对于确定不再使用的 owned String，可以移动绑定；不能把 `.bind(&str)` 描述为数据库全链路零分配，也不扩展为替换 Any。

## 4. B 的启用条件与实验设计

只有在 A+C 完成后，证据同时表明“同一大集合在一个工作单元内被多个 SQL 反复使用”以及“重复绑定/集合物化仍显著占用 CPU”，才启动独立 B 实验。

实验比较完整成本：`装载 + 建索引（如需）+ 全部 JOIN + 清理`，不能只计最后一个 JOIN。

- 固定同一物理连接/现有事务；不通过多次 `&pool` 调用假定访问同一临时表。
- 每连接初始化模板表；每次使用具有唯一工作标识和必要索引，防止取消/回滚后的数据与后续任务混用。
- 明确处理提交、失败、取消、连接归池和连接重建；临时数据不承载扫描恢复所需状态。
- SQLite/PostgreSQL 保持统一 storage 调用边界；后端 SQL 差异只封装在 storage 内部。
- 不为使用临时表新增覆盖整个扫描的长事务；记录连接占用和前台 p95。
- 不默认 `temp_store=MEMORY`，不假定临时表一定没有磁盘 I/O。

SQLite 临时表仅在创建它的连接可见。相比已经使用 VALUES CTE 的单次处理，B 没有预先成立的收益保证。未通过取消/连接复用测试或整体 A/B 无收益，则不进入生产实现。

## 5. 测量与验收

### 5.1 先建立基线

S0 为后续实现的前置条件，使用合成数据，不访问线上 FNOS 数据库。至少区分：

| 层级 | 工作负载 | 必须记录 |
| --- | --- | --- |
| Rust 微基准 | 静态 SQL、动态满批/变动尾批、PostgreSQL 编号；路径 32/128/512 字节 | SQL 构造次数/字节、分配次数/字节、耗时 |
| 单连接数据库 | 冷缓存与预热后分别执行；空、单条、满批、尾批、重复/不存在路径 | 结果一致、SQL 形状数、执行耗时、缓存驻留和构造缓存命中 |
| 正常连接池 | 同一合成目录与批次分布、并发作业 | 查询数、连接等待、扫描阶段 p50/p95、CPU 时间和 RSS |
| 60K 扫描 | 首扫、无变化重扫、约 1% 文件变化 | manifest 各阶段时间、总 CPU 秒、吞吐、前台浏览 p95 |

复用现有 `tests/performance.rs::lux_270_manifest_job_scan_benchmark` 和合成目录工具验证整链路；新增小型 ignored 库基准只用于隔离构造/分配和连接缓存行为。性能测试不在普通 CI 用墙钟硬阈值判断。

基准固定 release、数据、并发配置和批次分布；至少 5 轮，交错运行基线与候选，分别报告中位数和离散程度。冷/热数据库页缓存与 statement cache 不混为一个状态。共享 target 上的构建顺序执行，最终分别保存带提交标识的基线/候选可执行物，避免比较时误用最后一次构建结果。

进程分配计数与时间基准分开运行，避免计数器/采样器扭曲结论。可用独立基准进程的 allocator 计数或平台分配 profiler；不向生产路径添加全局 allocator，也不为了基准引入核心依赖。

`cached_statements_size()` 只表示驻留条数，不能当作 prepare 次数或命中率。真实准备成本通过可用的驱动/原生 prepare 采样确认；若当前工具不能测量，明确标为未测，不能用 SQL 文本缓存命中率替代。

SQLite CPU 包含在 Lux 进程内；PostgreSQL 还需单独记录数据库服务端 CPU。SQLx 层构造收益与数据库执行收益分别报告。

### 5.2 硬性正确性条件

- SQLite 转交静态、String、Arc SQL 时文本地址保持不变；测试在原实现先失败，再验证新实现。该断言只证明适配层未复制。
- PostgreSQL 占位符编号/绑定顺序和已有引号行为一致，覆盖多位编号、Unicode 与多个参数组；无占位符 SQL 不重建。
- 模板 key 无后端/版本串用；同一 key 命中共享 Arc；总参数数包含每行和固定参数。
- 借用输入和 SQL 回读路径混合时不漏记/重复记录 presence；重试、CAS 冲突、取消、增量扫描抢先写入仍保持一致。
- 写入 rows_affected、RETURNING、计数、序号和数据库最终内容与基线相同；新增数据库业务查询数不增加。
- 分桶覆盖容量两侧、最大边界、跨 chunk、重复、不存在路径；不会生成空 IN，也不会为写入添加假行。
- 缓存并发构造/淘汰/超限安全，预算有自动化断言；锁不跨 await。

### 5.3 性能判定

确定性目标：SQLite 适配层消除整条 SQL 复制；缓存命中时不再拼接模板或执行 PostgreSQL 占位符改写；相应应用临时 Vec/深复制消失。

实验目标：构造微基准耗时/分配有可重复下降；整链路 CPU 和扫描/前台 p95 无可重复回退。暂以超过 5% 且超出基线波动的回退作为调查/撤回该增量的触发条件；5% 是预设决策线，不是已测结果或性能保证。若 5 轮波动仍大于此值，增加样本或隔离干扰，不能判为通过。

不预先承诺整机 CPU 降低 3–5% 或任何固定比例。通过构造微基准只能证明局部收益；本机 macOS `arm64` 的结果不能外推 NAS/x86_64。

## 6. 分步实施清单

每个工作包实施时先登记独立 LUX 任务，先失败测试、最小实现、定向回归、原子提交。下表为各自的精确预计文件，均控制在 5 个以内；`LUX-DEVELOPMENT.md` 包含该步登记与验收记录。

| 工作包 | 依赖与结果 | 预计修改文件 | 主要验收 |
| --- | --- | --- | --- |
| S0：基线（LUX-562） | 无；建立局部 SQL 构造/分配与单连接 statement-cache 基线 | `src/storage/sql_query_benchmarks.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`、本文档 | 五轮 release 结果可复跑，数据/架构/缓存状态明确；本步不声称扫描整链路收益 |
| S1：查询入口 | S0；SQLite SqlStr 直传，PostgreSQL 编号无临时 String | `src/storage/repository.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md` | 地址保持测试先红后绿；编号与集成回归通过 |
| S2：模板基础 | S1；有限键、满批模板、单缓冲区构造 | `src/storage/sql_templates.rs`（新增，含单测）、`src/storage/repository.rs`、`src/storage/sql_query_benchmarks.rs`、`docs/LUX-DEVELOPMENT.md` | Arc 重用、绑定预算、并发和文本预算测试通过 |
| S3：两类读取 | S2；filesystem/diff 的模板与 IN 分桶 A/B | `src/storage/filesystem.rs`、`src/storage/jobs/scan/manifest/diff.rs`、`src/storage/sql_templates.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md` | 分桶两侧与跨批次结果一致；无收益则保留精确模板 |
| S4：发现路径与 presence | S2；精确 CTE 模板与局部 Cow | `src/storage/jobs/scan/manifest/discovery/paths.rs`、`src/storage/jobs/scan/manifest/discovery/presence.rs`、`src/storage/sql_templates.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md` | workflow/format 版本、完整成功/回读、重试计数均一致 |
| S5a：目录 DML | S2；目录模板，不改批次/事务 | `src/storage/jobs/scan/manifest/discovery/frontier.rs`、`src/storage/sql_templates.rs`、`src/storage/jobs/tests.rs`、`docs/LUX-DEVELOPMENT.md` | 重放与完成计数保持一致 |
| S5b：delta DML | S2；校验/插入/CASE 模板，删除片段 clone | `src/storage/jobs/scan/manifest/diff/insert.rs`、`src/storage/jobs/scan/manifest/apply/finalize.rs`、`src/storage/sql_templates.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md` | CAS、RETURNING、attempt_count、重试冲突均一致 |
| S6：文件记录借用 | S1；去掉正向索引电影/剧集深复制 | `src/storage/jobs/scan/manifest/indexes.rs`、`src/storage/media.rs`、`src/storage/repository_tests.rs`、`docs/LUX-DEVELOPMENT.md` | claim 胜者与既有媒体归并/剧集层级不变；分配下降 |
| S7：完成门禁 | 所有保留的增量；记录 A/B 与限制 | `docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md` | 完整 Rust 门禁、实际 PostgreSQL、性能结论可复查 |

S3–S6 按已测收益依次处理。`discovery/checkpoint.rs`、scan targets、reconciliation 等作为后续候选，只有基线显示仍是主要构造成本时另登记小任务；不默认把 215 处全部重写。B 也需单独任务，不因本文描述而提前实施。

### 6.1 定向检查与完成检查

所有 Cargo 命令前确认 `/Volumes/Toshiba` 实际挂载，并使用统一 target。LUX-562 已在该 target 上执行 ignored release 基准、`cargo build --locked`、`cargo fmt --all -- --check`、全目标全 feature Clippy 和全量 Rust 测试。后续实现包按各自的 LUX 任务运行以下窄测与门禁：

```bash
mount | rg '/Volumes/Toshiba'
export CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target
uname -m

# S1 及存储 helper 回归；必要时用对应库模块进一步收窄。
cargo test --locked --lib storage::repository

# S3–S6 按变化选择明确的集成目标。
cargo test --locked --test storage --test scanner --test scanning_jobs
cargo test --locked --test series_scanner --test mixed_scanner

# 有独立 PostgreSQL 测试实例时显式运行 ignored 测试。
cargo test --locked --test postgres_database -- --ignored --test-threads=1

# 完成门禁，非每个小增量执行。
cargo build --locked
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
```

S6 还应根据实际受影响的媒体/metadata helper 选择相关集成目标；所有新增边界回归必须由所选目标真实执行。PostgreSQL 用例当前标记 ignored，默认执行 `--test postgres_database` 并不能证明后端验证通过。

`./scripts/check-all.sh` 在完整项目/正式阶段门时执行；原有失败与本次新增失败分别复现记录。此次 Rust 存储优化本身不修改 Web。所有正式阶段门按项目规范由所有者确认后再进入下一阶段。

## 7. 风险、回退与交付边界

| 风险 | 控制与回退 |
| --- | --- |
| 分桶多绑定抵消 prepare 收益 | 按查询 A/B；单独撤回分桶，保留 SqlStr/模板复用 |
| 模板缓存内存、Arc 存活与锁竞争 | 有限 key、文本预算、在途峰值测量；不跨 await；超限无缓存执行 |
| Cow 生命周期传播扩大改动 | 仅 presence 内混合来源集合使用；其他位置用局部引用 |
| 文件记录借用影响预取/聚合 helper | S6 单独提交，跑电影、剧集、混合库及 claim 竞态回归 |
| 缓存或模板把不同版本/参数顺序混用 | key 包含版本/后端，逐模板测试参数数量、顺序和结果 |
| 生产 CPU 收益小于微基准 | 分层报告；不以局部分配下降替代 60K 扫描或 NAS 证明 |

每个增量独立提交且不改变 schema，可按逆序回退。设计不需要新核心依赖、migration 或公共 API 修改；也不处理报告第一至第四、第六至第八点，不推送、合并或部署。

LUX-562 仅建立局部 SQL 构造/分配与 SQLite statement-cache 基线，未更改生产查询。五轮数据、命令、普通构建/格式/Clippy 结果及全量 Rust 门禁的已复现基线失败记录在 `docs/PERFORMANCE.md` 和 `docs/LUX-DEVELOPMENT.md`。尚未测量扫描整链路或 NAS 性能，也未实施 S1 及后续优化。

## 8. 依据

- 项目事实来源：`docs/LUX-DEVELOPMENT.md` 第 24 节、LUX-246 的跨数据库/有界批次边界及当前扫描实现。LUX-246 仅提供约束参考，不代表本设计承接其全部未完成范围。
- 本机 Cargo registry 的锁定版本源码：`sqlx-core-0.9.0/src/sql_str.rs`、`sqlx-core-0.9.0/src/query.rs`、`sqlx-core-0.9.0/src/any/types/str.rs`、`sqlx-core-0.9.0/src/any/connection/executor.rs`；SQLite/PostgreSQL 驱动的 options、connection 和 statement-cache 实现。
- [SQLx 0.9.0 query 官方文档](https://docs.rs/sqlx/0.9.0/sqlx/fn.query.html)：查询的透明预编译缓存与参数绑定。
- [SQLite 临时文件与 TEMP 数据库说明](https://sqlite.org/tempfiles.html)：连接可见范围、生命周期与临时数据存储成本。
- [SQLite 参数等实现限制](https://sqlite.org/limits.html)：模板和分桶继续遵守当前项目的参数预算。
