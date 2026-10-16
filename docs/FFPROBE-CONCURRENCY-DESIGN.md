# ffprobe CPU 容量预算与完成速率自适应设计

状态：优化方向已由项目所有者认可，代码方案已细化，尚未实现。任务：LUX-561（仅设计）。日期：2026-10-16。

代码模块、内部接口、进程生命周期和逐步提交方案见
[`FFPROBE-CONCURRENCY-CODE-PLAN.md`](FFPROBE-CONCURRENCY-CODE-PLAN.md)。

设计基线：`test` 的 `1e7c2ea88d8cec5d2b8083bb4b9193b471d7af4d`。
分支：`codex/ffprobe-adaptive-concurrency`。

## 1. 目标与范围

将内置本地媒体探测从默认配置 256、CPU × 32 的有效目标，改为默认 CPU × 4 的
进程预算，并用实际 ffprobe 成功完成速率选择预算内合适的并发。
macOS 和直装 Linux/NAS 必须在没有 cgroup 使用率指标时正常工作。

本方案把“非 cgroup CPU 感知”定义为识别当前进程可用的 CPU **容量**，用于默认值和上限。
运行时控制不依赖 CPU 使用率、load average、`/proc/stat` 或外部监控服务。
不新增 `sysinfo` 依赖。容量感知与负载反馈是两个独立输入：前者给预算，后者由 probe 自己测量。

仅处理分析报告第一点涉及的内置本地 ffprobe 调度；不修改 fingerprint、Manifest/SQLite
事务、Discovery、本地 NFO worker、缩略图 ffmpeg 或跨管线 CPU 预算。
扫描完成事件、目标持久化、仅探测变化 source、路径验证和 STRM 旁车回退保持原合同。

STRM 远程探测插件当前使用自己的 operation semaphore 和 `background_concurrency`，
不经过 `GLOBAL_PROBE_SLOTS`。本次不改它的插件协议或远程任务调度；本文的进程预算
准确指所有 `MediaProbeService` 实例共用的**本地 ffprobe**预算，不能宣称覆盖插件派生的所有进程。
读取 STRM 的现有 JSON/NFO 旁车不启动 ffprobe，不计入此控制器。

成功条件是并发有界、无 cgroup 可运行、吞吐驱动的控制可验证，随后用基准评估 CPU 与前台延迟。
报告中“CPU 降低 50–70%”等数值未在本任务测量，不作为既有事实或收益承诺。

## 2. 当前实现核对

| 位置 | 已确认行为 | 对设计的影响 |
| --- | --- | --- |
| `src/application/probe.rs` | 默认 256；绝对上限 512；静态全局 semaphore | 默认计算与全局准入必须一起调整 |
| `src/application/libraries.rs` | 新库持久化 `probeConcurrency=256` | 仅改运行时 fallback 不会改变新库默认值 |
| `src/observability/resources.rs` | 有 CPU 样本时目标可到 CPU × 32；无样本时持续受 `min(CPU × 2, 16)` 限制 | 报告所说“非 cgroup 首次之后快速增长至 CPU × 32”与当前源码不符：4 核无样本通常一直是 8 |
| `probe_movie_library` / `probe_scan_job` | 每个最多 500 条的 page 开始时计算并发；page 内固定 | 反馈必须进入 page 内的补位/准入，不能等下一页 |
| `probe_source_with_slot` | local ffprobe 与旁车路径一起获取 slot，未记录实际探测完成速率 | 将实际进程执行与旁车读取分开计量 |
| `Dockerfile` / `compose.yaml` | 默认注入 `LUX_PROBE_CONCURRENCY=8` | 若保留，会遮蔽 CPU 自动默认值 |
| 规格 §3.14、管理健康测试 | 仪表盘 CPU/内存只允许容器来源 | 不给 dashboard 填入宿主机使用率，不改变 `source=cgroup` 的合同 |

规格 §5.4、§6.3 和 LUX-197 仍记录旧策略。本次登记设计，不将现行行为提前标记为已切换；
实施对应任务时再更新规范性条文，保留 LUX-197 的历史验收及基准记录，并链接替代任务。

## 3. CPU 容量与配置语义

### 3.1 识别路径

1. 非 cgroup 平台使用 `std::thread::available_parallelism()` 的可用并行度估计，失败按 1 核处理。
   Linux 需要考虑进程 affinity/cpuset；macOS 无需伪造 cgroup 数据。
2. cgroup v2/v1 存在时，独立读取可用 CPU 配额及 cpuset 限制，与可用并行度估计取保守交集。
   配额检测不以 `cpu.stat` 成功或已有使用率差分样本为前提。
   可见父级配额也构成上限；无法读取的限制明确记录为未知。
3. `max`、无效、零/负配额不作为有效容量；有效小数配额保留到乘 4 之后再取整。
   标准库整数估计若更保守，接受更低预算，不向上猜测隐藏容量。
4. 启动时解析并缓存，活动期间至多每 30 秒通过有界异步/阻塞边界刷新。
   不在每个 source 的热路径反复查询系统。暂时读取失败保留上次有效值；首次全部失败使用 1。
   可见容量缩减立即收紧后续准入；增大只提高预算，仍需吞吐试探才增加活动并发。

容量来源记录为内部诊断枚举（cgroup / available_parallelism / fallback），不塞入健康 API。
标准库只是容量估计，不反映当前系统负载；其查询本身也不应放在热路径。
参见 [Rust 官方说明](https://doc.rust-lang.org/std/thread/fn.available_parallelism.html)。
配额与 cpuset 的含义参见 [Linux cgroup v2 官方文档](https://docs.kernel.org/admin-guide/cgroup-v2.html)。

### 3.2 默认与覆盖

```text
C = 探测到的正数 CPU 容量（cores）
auto_budget = clamp(ceil(C × 4), 1, 512)
process_budget = LUX_PROBE_CONCURRENCY 的显式数值，否则 auto_budget
library_limit = 已保存的 probeConcurrency；新库默认 auto_budget
admission_limit = min(process_budget, controller_limit)
library_inflight <= min(library_limit, admission_limit)
所有本地 ffprobe 的 active 总数 <= admission_limit
```

| 可用 CPU 容量 | 自动预算 | 建议起始活动并发 |
| --- | ---: | ---: |
| 1 | 4 | 1 |
| 2 | 8 | 2 |
| 4 | 16 | 4 |
| 8 | 32 | 8 |
| 16 | 64 | 16 |

起始活动并发为 `min(process_budget, max(1, ceil(C)))`；预算是上限，不要求立即启动这么多进程。
若容量探测结果为 0.5 / 1.5 核，对应自动预算为 2 / 6；若另一有效限制更低，采用更低值。
极大核数也保留绝对 512 上限。

兼容决策：

- `probeConcurrency` 保持整数类型与 1–512 输入范围，是每库上限。旧库的 256/512 不批量改库，
  未显式设置进程覆盖时仍受 CPU × 4 进程预算约束，无需 schema migration。
- `LUX_PROBE_CONCURRENCY` 是进程预算的人工覆盖，不是强制活动数；允许管理员明确设置比自动预算更高的数值，
  仍不得超过 512、绕过每库上限或关闭自适应。设置 1 可固定最多一个活动进程。
- 未设置或空白环境变量表示自动；非空的非整数、0、负数和 >512 仍按现有无效配置合同拒绝启动。
  环境变量在启动时固定，运行时不反复读取。
- Dockerfile 移除内置的 `=8`；Compose 保留显式值透传，默认传空，交由 Rust 解析为自动。
  用户已有的 `LUX_PROBE_CONCURRENCY=8` 仍生效；升级不会自动删除用户部署文件中的显式值。
- 新库默认存储创建时的自动预算；后续硬件变大不会自动抬高已保存的每库上限，硬件变小则进程预算立即约束它。

## 4. 完成速率控制器

引入进程共享的 `ProbeAdmissionController`，所有内置本地 probe 服务获取同一实例。
控制状态属于调度模块；`ResourceMetrics` 提供只读的前台延迟和内存保护输入，不再拥有 probe 的 CPU 使用率控制状态。
控制器保存固定容量的窗口样本与计数，不按媒体项永久存历史。

### 4.1 采样

- 在真正调用 `FfprobeRunner::probe_path` 前、拿到准入权之后开始计时；在进程退出且受限 JSON 解析完成后记结果。
  不包含排队、数据库写入、NFO 回写或旁车读取耗时。
- 主反馈 `rate = 成功完成的实际 ffprobe 数 / 有效窗口墙钟秒数`。记录 service time p95、active、峰值、
  饱和时间和等待需求；不能用累计完成数除以整个 job 时间，也不能用每项速率相加替代整体吞吐。
- 非零退出、spawn 失败、超时、取消、无变化跳过、STRM 旁车和 ffprobe 失败后的旁车成功不算 ffprobe 成功。
  即使数据库随后成功保存旁车，也不把它计成探测吞吐；每个真实 attempt 只记一次。
- 窗口初值 10 秒；成功不足 16 个时最长延长至 30 秒，仍不足则不据此升档。
  只有持续存在待处理工作、且至少 80% 窗口时间达到当前准入上限的窗口可比较吞吐。
  队列尾部、分页/数据库供给间隙和低每库配置造成的未饱和窗口不触发吞吐降档。
- 改档后的首个窗口预热；旧档在途 attempt 尚未退出则继续预热，避免把旧档产出归因到新档。
  活动任务记录档位 epoch；过期窗口和旧 epoch 样本不与当前档混比。
- 每秒有轻量 tick，只检查已有计数与时间，确保“没有完成”时也能检查停滞和取消。
  没有活动或等待工作时不持续轮询；重新出现工作再启动观察。

### 4.2 调节规则（首版参数，待基准校准）

| 状态 | 动作 |
| --- | --- |
| 起步 | 从约 CPU × 1 开始，收集稳定基线 |
| 稳定且仍有积压 | 每次仅试探 `N + max(1, ceil(N / 8))`，不超过预算和可用需求 |
| 试探成功 | 两个有效观察窗口均比原基线吞吐提升 ≥5%，且 service time p95 增幅 ≤20%、超时未恶化，保留新档 |
| 无收益或恶化 | 回到试探前档位；升档冷却 30 秒，避免在平台区来回扩张 |
| 稳定档吞吐恶化 | 连续两个饱和窗口较近期同档基线下降 ≥15%，按约 25% 下调并重新建立基线 |
| 严重停滞 | 有积压且有进程持续运行 30 秒却无成功完成，减半后续准入；原探测超时与回收仍负责在途进程 |
| 反向检查 | 稳定运行 60 秒后，允许试探降低一小档；若吞吐仍在原档 95% 以上，保留较低档，否则恢复并冷却 |

一次只进行一个试探，参考基线必须是紧邻的有效窗口，不用数分钟前的历史峰值。
严重停滞每 30 秒最多降一次，不取消健康在途任务。持续快速失败且无有效成功样本时，禁止升档，
回到不高于起始并发的安全档；分类计数保留用于诊断，不能把快速报错视为容量变好。
空闲 60 秒或容量/压力保护发生明显变化后清空比较基线，恢复时从安全档重新试探。

吞吐不增长时保留更低并发，是本设计节省 CPU/进程切换开销的主要机制。
CPU 使用率高但吞吐仍增加时，仅靠完成速率无法证明机器仍有空闲；因此 CPU × 4 预算和前台保护不可省略，
也不能承诺某个固定 CPU 百分比上限。

### 4.3 保护条件

移除 probe 对 `cpu.usage_percent` 的依赖，也移除“没有 CPU 样本就永久卡在启动上限”的分支。
现有内存和前台 p95 降档保留为保护性上限：近期前台 p95 ≥1 秒或容器内存 ≥85% 时，
预算降至至多一半；p95 ≥2 秒或内存 ≥95% 时，降至至多四分之一。
下降即时生效，恢复需要压力解除、30 秒冷却和新的有效吞吐观察，不直接翻倍回去。
前台样本需要时间戳，只取最近 30 秒；无新样本不重复使用陈旧高延迟永久压档。
非 cgroup 内存未知时不伪造宿主机数据，吞吐控制仍可独立运行。

## 5. 准入与执行集成

用 `Mutex<State>` + `Notify` 管理动态活动上限，替代目前固定大小的 local probe semaphore。
锁内只处理计数和小量窗口计算，不等待进程或 I/O；获取准入的检查与 active 增加原子完成。
先注册通知再检查条件，醒来循环复核，覆盖丢唤醒和多等待者竞争。

许可随子进程拥有者任务持有：完成、异常、JoinSet abort 和取消后，均在确认退出/回收后释放一次；
active 归零/许可释放/升档唤醒等待者。仅请求终止或等待 1 秒超时不足以归还配额，具体清理协议见代码方案。
降档不杀在途进程；若 active 暂时高于新上限，等待自然排空，此时禁止新增启动。
permit 的生命周期覆盖真实子进程退出/回收，必须验证取消时不会先释放预算、旧进程仍存活。
原有 30 秒探测超时、输出上限与子进程终止行为保留。

`probe_source_page` 在每次补位和控制器通知后重读当前限额，仍保持每页最多 500 项和有界 JoinSet；
不一次性创建整页乃至整库的等待任务。各库请求只约束各自的活动数，不将低配置库的限额写入全局状态。
多个库和多个服务实例共享一个活动预算；等待者取消能及时退出，无锁跨越 `.await`。
测试通过显式注入独立 controller/clock/capacity source 隔离，不在同一测试进程反复修改全局环境变量。

## 6. 诊断与验证

启动记录 CPU 容量来源、auto/process budget、环境覆盖是否存在；仅在改档时输出结构化诊断，
包含 old/new、reason、successful rate、service p95、active 和 budget。不记录媒体路径、URL 或 ffprobe stderr。
不新增公共 API、dashboard 宿主资源字段或对外遥测。

确定性测试先行：

- 容量：无 cgroup 的 1/2/4/8 核、读取失败、v1/v2、小数配额、quota/cpuset/affinity 交集及动态缩减。
- 配置：未设/空值自动；显式 1/8/512；无效输入；新库默认及旧库 256 在 4 核下最多 16。
- 控制器：吞吐线性增长、平台、退化、混合快慢任务、空队列、供给不足、全失败/超时、无完成停滞、冷却、反向试探。
  使用可注入时钟与 runner/gate；控制逻辑单测不依赖真实睡眠或本机核数。
- 执行：两个库及不同服务实例合计不超限；page 内降档立即限制新启动；取消、panic、超时、无进程回退均不泄漏 permit，
  真实子进程退出后才复用预算；低配置库不会把其它库全局档位锁死。
- 回归：旁车回退仍成功但不算 ffprobe 成功；STRM 不主动探测；未变 source 不重复探测；健康 API 继续只声明 cgroup 来源。

性能测量分两层：先使用同一真实调度入口的可控 I/O 延迟/CPU 工作量模拟探测，验证控制器可重复收敛；
再对固定、无敏感数据的本地媒体集运行真实 ffprobe。分别比较旧版默认、新版自动和若干固定并发档，
记录 items/s、p50/p95、总耗时、活动峰值、进程及子进程 CPU、前台 p95、失败/超时计数。
CPU 使用率只作事后验收证据，不进入控制算法。容器与非容器分别记录；ARM64 不能代替 NAS/x86 数据。
每组至少 3 次，固定数据集/ffprobe 版本与缓存条件；报告中位数和波动，基准中模拟最优档附近的吞吐目标为 ≥95%，
不把该目标冒充真实 NAS 的实测结果。真实样本若出现明显吞吐回退，必须记录原因并校准参数。

## 7. 后续实施拆分

当前 LUX-561 产出优化设计、代码方案与任务登记。下表为最初范围概览；精确拆分以代码方案 P1–P10 为准。
每单元先失败回归、最小实现、定向验证和原子提交；进入实现前重查该单元的文件预算。

| 顺序 | 可独立验证的结果 | 预计文件（每单元不超过 5 个） |
| --- | --- | --- |
| A | CPU 容量、自动预算和新库默认有确定性回归；旧库有运行时预算保护 | `src/observability/resources/cpu_capacity.rs`（新）、`src/observability/resources.rs`、`src/application/probe.rs`、`src/application/libraries.rs`、`tests/library.rs` |
| B | 成功完成速率控制器及动态准入接入本地 runner，page 内生效 | `src/application/probe/concurrency.rs`（新）、`src/application/probe.rs`、`src/observability/resources.rs`、`tests/probe.rs` |
| C | Docker/Compose 默认进入自动模式，显式覆盖兼容 | `src/config/mod.rs`、`Dockerfile`、`compose.yaml`、`docs/DEPLOYMENT.md`、`docs/LUX-DEVELOPMENT.md` |
| D | 多库、取消/超时及扫描兼容的执行证据 | `tests/probe.rs`、`tests/scanning_jobs.rs`、`tests/admin_health.rs`、`tests/admin_dashboard.rs`、`docs/COMPATIBILITY.md` |
| E | 可重复基准、参数校准与完成记录 | `tests/performance.rs`、`docs/PERFORMANCE.md`、`docs/LUX-DEVELOPMENT.md`、`docs/FFPROBE-CONCURRENCY-DESIGN.md` |

A → B → C → D → E 顺序实施，B 内先验证纯控制算法，再接准入/真实进程；如任一单元超出文件预算则继续拆分。
代码示意遵循现有 `ProbeError` / `Result` 边界，命名表达容量、预算、实际活动数的区别，不引入生产 `unwrap/expect/panic`。

实现期最窄检查按改动选择，不在每个增量运行全目标：

```bash
mount | rg '/Volumes/Toshiba'
uname -m
export CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target

cargo test --locked --lib observability::resources
cargo test --locked --lib application::probe
cargo test --locked --lib config::tests
cargo test --locked --test library --test probe
cargo test --locked --test scanning_jobs --test admin_health --test admin_dashboard
cargo test --release --locked --test performance lux_197_ffprobe_concurrency_benchmark -- --ignored --nocapture --test-threads=1
```

只有确认 Toshiba 挂载后才执行 Cargo；共享 target 有其它 Cargo 活动时先协调，不能并行覆盖。
任务完成运行 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target ./scripts/check-all.sh`，
其中包含 Rust build/all-targets/fmt/all-features Clippy 和项目要求的 Web 检查；如有既有失败，记录证据且不宣称全绿。
本次设计没有执行这些实现/性能检查，也没有访问 FNOS 或变更部署。

## 8. 审阅重点

以下是本方案明确选择的语义，实施前可调整：CPU 感知指容量而非新增宿主使用率采集；
CPU × 4 是未配置时的进程预算；显式环境变量可提高或降低预算；内置本地 ffprobe 的控制器不跨入远程插件调度。
观察窗口、5% 收益门槛、15% 退化门槛和冷却为待基准验证的首版参数，不是现成实测结论。
