# ffprobe 并发优化：代码实施方案

状态：实施中。LUX-561 已登记为实现任务；P1–P7c 已落地，P8–P10 仍待完成，后续按本方案逐个完成并提交。日期：2026-10-16。
优化方向已由项目所有者认可；本文件将
[`FFPROBE-CONCURRENCY-DESIGN.md`](FFPROBE-CONCURRENCY-DESIGN.md)
落实为代码职责、内部接口、执行顺序与失败回归。
分支为 `codex/ffprobe-adaptive-concurrency`，代码基线仍为 `1e7c2ea8`。

## 1. 确定的实施原则

- 自动进程预算为 `clamp(ceil(effective_cpu_cores × 4), 1, 512)`，起始活动数约 CPU × 1。
- 自动调节只使用真实本地 ffprobe 成功完成速率；CPU 使用率不进入控制状态机。
- 一个进程只有一个本地 probe 准入控制器；多个库、服务实例、clone 共用生产预算。
- 改档在 page 内影响后续启动；原有最多 500 项分页和持久化行为继续成立。
- 取消请求与“子进程已退出”分别处理：确认退出/回收后才归还活动配额。
- 不新增依赖、数据库字段、HTTP 接口或外部监控；保留远程 STRM 插件的独立调度。

本次核对发现 `config::parse_probe_concurrency` 已将未设及空白值解析为 `None`。
沿用它，补齐空白值回归即可，不重写解析器或另造 `auto` 字符串。

## 2. 文件与职责

| 文件 | 代码职责 |
| --- | --- |
| `src/observability/resources/cpu_capacity.rs`（新增） | CPU 容量探测、cgroup 配额/cpuset 解析、保守交集、30 秒缓存与自动预算纯函数 |
| `src/observability/resources.rs` | 内部容量入口、近期前台延迟/内存保护快照；退出旧 probe CPU 使用率调度路径 |
| `src/application/probe/adaptive.rs`（新增） | 纯状态机：观察窗口、试探、回退、冷却、停滞；只接收数值和单调时间 |
| `src/application/probe/concurrency.rs`（新增） | 共享准入、活动计数、页面需求登记、等待/唤醒、许可及关闭/排空 |
| `src/application/probe/runner.rs`（新增） | 提取现有 runner、进程输出收集和错误类型，集中维护进程生命周期 |
| `src/application/probe.rs` | 继续负责分页、路径验证、旁车回退和结果落库，连接上述模块 |
| `src/application/libraries.rs` | 新库默认改为创建时的自动预算，不改已有库数据 |
| `src/api/legacy.rs` | 只在已有后台关停入口加入本地 probe 关闭/排空 |

`probe.rs` 通过 `pub use runner::{FfprobeRunner, ProbeError}` 保持原 Rust 导入路径和公开方法签名。
`MediaProbeService::new`、`with_resource_metrics`、`probe_movie_library`、`probe_scan_job` 的现有调用点保持兼容。
新增构造/注入接口放在私有模块或 `#[cfg(test)]` 单测内，不为测试开放公共控制器 API。

## 3. CPU 容量与初始化

容量结果用内部值类型表达，`effective_cores` 经构造入口保证有限且大于零：

```rust
struct CpuCapacity {
    effective_cores: f64,
    source: CpuCapacitySource,
    observed_at: Instant,
}

async fn probe_cpu_capacity() -> CpuCapacity;
fn automatic_probe_budget(capacity: &CpuCapacity) -> usize;
```

以上为接口草图，不是已编译实现。探测层不依赖 application、数据库或媒体路径。
`available_parallelism` 在 `spawn_blocking` 边界调用；Linux 的有界 cgroup 文件读取使用异步文件入口。
缓存由 `OnceLock` 中的共享状态持有，刷新采用一次只允许一个调用者执行的锁；缓存新鲜时直接取快照。
其它调用者在已有有效值时使用旧快照，不为同一轮刷新创建更多任务。

新库创建和 probe 首次运行复用容量缓存。`LibraryService::new` 和 `MediaProbeService::new`
保持同步，不在构造器中做文件 I/O、启动监控任务或要求存在 Tokio runtime。
新库在异步 `create_library_with_scrapers_and_chapter_source` 写库前取得自动预算。

cgroup 的 v1 controller、v2 membership 和挂载路径要分开解析，不能把当前
`cgroup_relative_path()` “取首个控制器路径”的逻辑直接复制为容量解析器。
沿可信挂载根到当前 cgroup 检查可见祖先限制；路径禁止向上逃逸，读取深度/字节数有上界。
祖先本身不一定包含当前 PID，不能用 `cgroup.procs` 必须包含 PID 的条件去判断祖先配额。
覆盖 `cpu.max=max`、v1 quota=-1、分数核、cpuset ranges、权限失败和容器 namespace 重映射。
检测不到的限制标为未知并保守使用标准库估计，不将“读取失败”解释为机器无限大。

配置覆盖在首次准备进程控制器时解析并固定；错误沿现有 ConfigError / ProbeServiceError::Worker 边界传播。
`Config::from_env` 的启动校验保留，不在每次补位时重读环境变量。
有显式数值时预算继续取该值，容量仍决定起始档位；无覆盖时预算跟随容量变化。

## 4. 纯算法状态机

`adaptive.rs` 不包含 async、spawn、数据库、ResourceMetrics 或 cgroup 调用。
调用者显式传入 Instant，测试以起点加 Duration 驱动，不增加 Tokio test-util feature。

```rust
enum AdaptivePhase {
    Warmup,
    Stable,
    TrialUp,
    TrialDown,
    Cooldown,
}

struct AdaptiveState {
    current_limit: usize,
    budget: usize,
    epoch: u64,
    phase: AdaptivePhase,
    // 当前窗口、紧邻基线、试探前档位、冷却/停滞时间。
}

impl AdaptiveState {
    fn evaluate(&mut self, now: Instant, input: WindowInput) -> Option<Adjustment>;
}
```

窗口初值 10 秒、最低 16 个成功样本、最长 30 秒；上调约 1/8，至少增加 1。
升档保留要求两个有效窗口吞吐增益均 ≥5%、采样 service p95 增幅 ≤20%，否则回退并冷却 30 秒。
同档连续两个饱和窗口恶化 ≥15% 时下调约 1/4；无成功完成 30 秒时减半后续准入。
稳定 60 秒后可反向试探小幅降档，吞吐保留 ≥95% 则保留低档。

窗口记录成功数、失败/超时/取消数、饱和时长和固定容量的延迟样本。
延迟环形缓存上限初值 256；其 p95 是采样估计，性能报告的全量 p95 由 benchmark 单独统计。
active 或需求变化时先累计上一段时间的饱和时长，不能只在完成瞬间数“是否满载”。
判断饱和用当时实际可执行的总上限；每库配置及待处理需求不足时，不把预算未跑满误判为吞吐退化。

目标变更增加 epoch；旧档 attempt 排空、首个预热窗口结束后才开始同档比较。
每次只允许一个试探；窗口不足、队列尾部和供给中断不升档、不做吞吐下降判定。
压力/容量或活动库组合变化使当前比较失效，重新预热，降低混合媒体负载变化的误判风险。
状态枚举可带基线与试探数据，避免多个 bool 形成非法组合。
这些参数集中定义为常量，首版不增加更多环境变量；基准不达标再校准。

## 5. 动态准入和 page 调度

`concurrency.rs` 内部组织共享状态，示意如下：

```rust
struct ProbeRuntime {
    state: Mutex<AdmissionState>,
    changed: Notify,
}

impl ProbeRuntime {
    async fn acquire(self: &Arc<Self>, session: &ProbeSession) -> Result<ProbePermit, ProbeServiceError>;
    fn update_demand(&self, session: &ProbeSession, remaining: usize);
    fn tick(&self, now: Instant, pressure: ProbePressure);
    async fn close_and_drain(&self);
}
```

上述接口均为内部接口。生产实例由进程 OnceLock 共享，其中不保存永久绑定某个 Tokio runtime 的定时任务。
page 的 select 循环以 1 秒 tick 驱动时间检查；控制器按时间去重，多个库不会重复执行同一秒的调节。
CPU/内存刷新按各自缓存周期合并；每次 refill 不做系统采样。

ProbeSession 是一轮 page 的内部登记，持有库 ID、库配置上限、剩余本地文件需求及保护快照。
登记随 page 释放；同库并行 session 的有效库上限取它们配置快照的较小值，active 按库汇总，
从而不会因同库启动两个服务实例而绕过每库上限。
不同库互不压低对方配置，只有进程预算和全局保护共同约束总数。
登记的数据量受活动 page 数限制，不为整个库或每个 source 长期存一条记录。
session Drop 只撤销需求和等待登记；仍在运行/回收的进程计数必须由其许可释放，不能跟着 page 一起清零。
库 ID 使用现有 LibraryId 新类型；controller 不持有路径、source 记录或 SQL 访问入口。

准入在一个短锁内完成“检查关闭/进程上限/库上限 + 增加 active”。
降档期间若已有 active 大于新上限，禁止新增启动，等待自然排空。
Notify 只负责唤醒，醒来后重新检查；在检查前创建并注册通知 future，避免丢唤醒。
不在持锁时 await、读文件或等待子进程。不承诺严格 FIFO，但多库有限工作量测试必须全部完成，不能遗漏唤醒。

`probe_source_page` 从当前 for + while 改为有界的 refill/select 循环：

1. 输入仍只保留一个最多 500 条的 page；READY/非 PENDING 沿用原跳过逻辑。
2. 每次补位重读当前库和进程限额；JoinSet 规模受当前有效上限约束，降档不强行 abort 已启动任务。
3. 等待 join_next、控制器变更或 1 秒 tick；升档通知可在没有新完成结果时允许补位。
4. 控制器可见尚未提交的本地文件需求及准入等待，不把“JoinSet 已完成但尚未落库”的结果计为活动 ffprobe。
5. 收到结果后继续原 persist_probe_attempt；不引入批量写库、改 target 状态或调整完成事件。

一个 page 不把剩余 500 项全部 spawn 成等待者。STRM 旁车走现有有界 page 工作槽，
不占真实 ffprobe 活动许可，也不进入吞吐成功计数。
原 effective_concurrency 中 CPU 使用率路径及静态 GLOBAL_PROBE_SLOTS 在新路径接通后删除，
不保留两套相互影响的 probe 降档器。

## 6. 子进程拥有权与取消

runner 的公开 `probe_path(&Path)` 签名保留；服务通过新的私有执行入口传入 ProbePermit。
公开 runner 测试也复用相同的输出/清理逻辑，避免 timeout 和输出限制出现两套实现。

采用获得许可后才创建的单次进程拥有者任务，同时持有 Child、许可和结果 oneshot sender。
只有服务一侧持有 receiver。page 任务取消或 JoinSet abort 会丢弃 receiver，拥有者通过
Sender::closed() 察觉取消，执行终止和 wait；拥有者不加入会被 page abort_all 的 JoinSet。
任务数量由先取得的许可约束，绝对最多 512 个；不为尚未获准的 source 创建这种任务。

正常顺序：获得许可 → spawn → 有界读取 stdout/stderr 并 wait → 解析 JSON →
记录真实 attempt 结果 → 释放许可 → 发送结果 → 服务执行旁车回退/落库。
spawn 失败时未创建子进程，可立即记录失败并释放。

超时/取消/输出超限顺序：请求终止 → 等待确认退出/回收 → 记录对应 attempt 分类 → 释放许可。
start_kill 与 kill_on_drop 不等于确认回收；不能沿用当前 terminate_child
“最多等 1 秒然后忽略结果”的逻辑后直接释放许可。
如 1 秒内仍未确认，先向调用者返回原错误，Child 与许可继续留在清理任务中等待，期间不能复用该配额。
wait 报错但无法确认退出时保留占用并输出有节流的非敏感诊断，不推测进程已经消失。
测试先用可控回收 gate 证明等待未完成时许可不会复用，再以真实子进程验证终止与回收；
不靠取消后立刻检查 PID 的竞态来声称覆盖了所有提前释放情况。

拥有者中的 Child/许可由同一个清理 guard 持有。正常路径显式 finish；意外 unwind 时 guard 将两者
一起交给受同一许可约束的异步回收任务，避免普通 permit Drop 提前补位。
生产路径不新增 panic；回收任务不重复转交自身，避免递归清理或重复减计数。
此处必须先有真实 PID/gate 回归证明，再接入自适应。

ProbePermit 同时负责 attempt epoch 和 active，完成分类只提交一次。
调用者不在旁车回退或持久化路径补记成功；取消分支不进入 persist_probe_attempt，不将未完成目标写成成功。
本次不改变扫描作业取消 API 或数据库状态机，仅保证既有 future 取消、任务 abort 和服务关闭时正确清理进程。

在 AppState::shutdown_background_workers 中，probe 首先关闭新准入并唤醒等待者，通知已登记拥有者清理，
随后等待 active 排空，再允许正常关停流程进入数据库关闭。入口保持幂等。
如果 OS 尚未确认某进程退出，状态必须保留为未排空；不能仅因收到终止请求就报告完成。
进程终止或 runtime 销毁之后的 OS 清理由平台负责，不把那种情况当成可继续准入的正常执行状态。

## 7. 保护信号与错误边界

ResourceMetrics 提供私有 probe_pressure 快照，只含近期 home p95、容器 memory 百分比及采样时间。
home 样本在原有有界 ring 内增加时间戳；原 home_latency_p95_ms 保持原读取语义，
新增 probe 专用最近 30 秒读取，避免顺手改变 metadata/background 降档行为。
内存采样最多每 5 秒合并一次。多个活动 session 的有效保护快照取最保守档位；过期快照不永久保留压力。
with_resource_metrics 只绑定本服务的指标来源，不重建或重置共享控制器。

继续使用现有 ProbeError、ProbeServiceError 和 target 状态对外边界。
控制器内部使用成功/失败/超时/取消枚举分类；关闭导致的内部停止通过控制流处理，
不添加公共 JSON 字段、Emby 状态码或新的数据库状态。
失去锁保护时返回现有 Worker/Io 错误，不能降级为无配额执行。
日志只记录容量来源、数值、档位原因和计数，不记录媒体路径、URL 或原始 stderr。

## 8. 按依赖拆分的实施单元

以下只安排代码任务；实施前逐个登记 LUX 编号，每次只执行一个。纯移动增量与行为变化分开提交。
表中文件数包含后续实现预计修改的规范记录；如实际超预算，先继续拆分。

| 单元 | 预计文件（最多 5 个） | 第一条验证与完成条件 |
| --- | --- | --- |
| P1：容量与自动预算 | `src/observability/resources/cpu_capacity.rs`、`src/observability/resources.rs`、`docs/LUX-DEVELOPMENT.md` | CPU/cgroup fixture 单测先失败；无 cgroup、分数核、未知限制和刷新均可确定性验证 |
| P2：默认值与旧库保护 | `src/application/libraries.rs`、`src/application/probe.rs`、`tests/library.rs`、`docs/LUX-DEVELOPMENT.md` | 新库不再存 256，旧库实际预算受限，显式覆盖仍生效 |
| P3：runner 提取 | `src/application/probe.rs`、`src/application/probe/runner.rs`、`docs/LUX-DEVELOPMENT.md` | 先跑现有 probe 集成目标再纯移动；公开路径/行为保持一致 |
| P4：子进程回收 | `src/application/probe/runner.rs`、`tests/probe.rs`、`docs/LUX-DEVELOPMENT.md` | 先复现取消 future 未确认回收的问题；超时/输出限制/取消均有真实进程验证 |
| P5：纯算法 | `src/application/probe.rs`、`src/application/probe/adaptive.rs`、`docs/LUX-DEVELOPMENT.md` | 注入时间和吞吐序列，验证试探/平台/下降/预热/冷却；不接生产执行 |
| P6：准入与 page 补位 | `src/application/probe.rs`、`src/application/probe/concurrency.rs`、`src/application/probe/adaptive.rs`、`src/application/probe/runner.rs`、`docs/LUX-DEVELOPMENT.md` | 私有模块单测注入 fresh runtime；多库/同库多实例受控，取消回收前不补位，page 内生效 |
| P7a：资源快照 | `src/observability/resources.rs`、`docs/LUX-DEVELOPMENT.md` | probe 专用近期 p95 和 5 秒合并内存快照；原 dashboard 读取语义不变 |
| P7b：压力控制 | `src/application/probe/adaptive.rs`、`src/application/probe/concurrency.rs`、`docs/LUX-DEVELOPMENT.md`、本方案 | 多会话取最严重档位；过期/恢复重置吞吐基线；仅压力升高时保护性下调 |
| P7c：页面接线与关停 | `src/application/probe.rs`、`src/application/probe/concurrency.rs`、`src/api/legacy.rs`、`src/main.rs`、`docs/LUX-DEVELOPMENT.md` | 每页刷新压力；关闭新准入并唤醒页面；已有 runtime、页面会话及活动子进程排空后才关闭数据库 |
| P8：容器默认 | `src/config/mod.rs`、`Dockerfile`、`compose.yaml`、`docs/DEPLOYMENT.md`、`docs/LUX-DEVELOPMENT.md` | 仅补现有 parser 空白值测试；不再固定注入 8，显式数值透传 |
| P9：集成与兼容 | `tests/probe.rs`、`tests/scanning_jobs.rs`、`tests/shutdown.rs`、`docs/COMPATIBILITY.md`、`docs/LUX-DEVELOPMENT.md` | scanner/服务调用链、旁车、变化目标及关闭子进程回归通过 |
| P10：基准与完成 | `tests/performance.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`、两份 ffprobe 设计文档 | 模拟及真实探测 A/B，校准参数并记录全量门；不使用未测量 CPU 降幅 |

P4 先确定 runner 拥有权及回收协议；P6 才让新许可随该协议一起传入。
P2 在旧执行器保留过渡性的 CPU × 4 预算保护；P6/P7 接通新路径时删除旧 probe CPU 降档器，避免中间提交无上限。
P6/P7 所需 service 级隔离测试放在同模块 cfg(test) 下，可访问私有构造器，不新增公开测试 seam。
进程全局关闭回归放在已有 tests/shutdown.rs 的子进程模式中，不在同一测试进程关闭/重建全局 singleton。

实现期最窄检查为 `cargo test --locked --lib observability::resources`、
`cargo test --locked --lib application::probe`、`cargo test --locked --lib config::tests`，
以及显式 `--test probe`、`--test library`、`--test scanning_jobs`、`--test shutdown`、
`--test admin_health`、`--test admin_dashboard`，按当前单元选择，不能每轮全目标。
完成时执行项目全量门及 ffprobe 性能目标。所有 Cargo 命令先确认 Toshiba 挂载，
设置 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target`，避免与共享 target 的其它 Cargo 任务并发。

## 9. 本轮核对证据与状态

- 已核对 service、runner、资源采样、新库创建、容器默认和 AppState/扫描后处理的现有调用链。
- Cargo.lock 锁定 Tokio 1.53.1；回收/通知/oneshot 语义同时核对本机该版本的 registry 源码。
  官方在线资料为 [Child](https://docs.rs/tokio/latest/tokio/process/struct.Child.html)、
  [Notify](https://docs.rs/tokio/latest/tokio/sync/struct.Notify.html) 和
  [oneshot Sender](https://docs.rs/tokio/latest/tokio/sync/oneshot/struct.Sender.html)；在线最新版本与 lockfile 不完全相同，
  实现以锁定版本及其编译/回归结果为准。
- 实现进度：P1 已加入标准库 CPU 容量、Linux cgroup v1/v2 quota/cpuset 交集、30 秒缓存及自动预算计算；P1 macOS 定向单测 6/6 通过，其中包含 cgroup 解析 fixture。P2 已改新库默认和共享进程预算，CPU 自动预算/环境覆盖单测 1/1、新库持久化单测 1/1、`library` 13/13、`probe` 19/19 通过。P3 已将 runner、错误类型和有界子进程输出读取移动到独立模块，`probe` 19/19 通过。P4 让独立 runner owner 持有子进程，并在取消、timeout 或输出错误后持续持有资源直到 `Child::wait` 确认退出；调用者可先收到原错误，后台 reaper 继续持有 permit。wait gate 单测先按旧逻辑失败，改后通过，取消子进程的真实 PID 检查和 `probe` 20/20 通过。P5 完成纯速率状态机，涵盖预热、饱和资格、两窗升档、p95/timeout 回退、冷却、退化、停滞、失败安全档及反向降档；定向单测 13/13 通过。P6 已接入进程级共享准入、同库并发上限与 page 补位；多库/同库 session、预算下降、停滞降档、Notify 不丢唤醒、真实 permit wait gate 和取消下真实子进程均有回归覆盖。probe 模块单测 26/26、`probe` 集成测试 20/20、格式检查和全 targets/all features Clippy 通过。P7a 提供近期首页 p95 与 5 秒合并的容器内存压力快照，原 dashboard p95 语义不变。P7b 接通压力分类、多会话最保守聚合及过期/恢复基线重置，不读 CPU 使用率。P7c 在页内接入压力快照，并在 AppState 关停时关闭 registry、拒绝迟到 runtime、唤醒并排空所有页面会话及活动子进程许可；旁车空闲页过早排空回归先失败后修复。P7 控制器单测 11/11、probe 模块单测 32/32、资源模块单测 23/23、`probe` 20/20、`shutdown` 1/1 通过。Linux 目标交叉构建因缺少 `x86_64-linux-gnu-gcc` 未能启动。P8–P10、性能测量和完整完成门尚未执行。
