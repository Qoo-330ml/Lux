# AppState 就绪状态类型化（第二阶段设计提案）

日期：2026-10-30。状态：**方案 A 的步骤 1、2 已实施**（见 §6），步骤 3–5 未实施。范围：`src/api/**`。
第一阶段（纯机械收敛）已完成，本文件给出把"就绪/未就绪"从 `Option` 升级为类型状态的方案、改动面、
迁移步骤与风险，并记录步骤 1、2 的落地结果。

## 1. 第一阶段交付后的基线

- `AppState`（`src/api/legacy.rs:169`）共 62 个字段，其中 **49 个 `Option<...>`**（46 业务服务 + 3 非服务 core，见 §3）。
- 已被 `require_service!(state.<field>)` 收敛的"服务未初始化 → 裸 503"let-else：**308 处**（22 个文件，净减 548 行）。
- 仍是 let-else 的 **118 处**（统计：`grep -r "as_ref() else {" src/api | wc -l` 得 123，减去 3 处非 `state`
  接收者、`src/api/mod.rs` 的宏体与宏文档示例各 1 处；脚本按块分类）：59 处 `api_error(.., DatabaseUnavailable)`、
  39 处直接返回状态码（37 处裸 503 变体，另有 NOT_FOUND 5 / NOT_IMPLEMENTED 1）、18 处降级返回（`return;`、
  `Ok(None)`、`false`、默认值）、2 处带日志。另有 60 处 `return StatusCode::SERVICE_UNAVAILABLE.into_response();`
  出现在 `match` 分支或宏体里。
- 运行期仍是"一个 `Router` + `AppState`"（`app()` = `app_with_state(AppState::default())`）：宏只消除重复文本，不阻止漏检。

## 2. 目标

- setup / 未就绪模式：状态里**没有**业务服务，业务路由**不注册**（而不是"注册了但返回 503"）。
- ready 模式：46 个服务字段**不是 `Option`**，handler 直接拿 `&Service`，308 处守卫在编译期消失；运行期故障仍走现有 503。

## 3. 字段划分（62 个，核对自 `src/api/legacy.rs:169`）与两个方案

- `CoreState` 共 **16 个**：13 个必需字段（`admin_events`/`user_events`/`resources`/`request_metrics`/
  `remote_access`/3 个限流器/`sse_permits`/`admin_health_probe_cache`/`server_id`/`filmly_image_compat_mode`/
  `database_selection_required`）+ 3 个**非服务** Option（`database`、`config_dir`、`restart_handle`）。
- `ReadyServices` 共 **46 个**：44 个 `Option<Service>` + 2 个 `Option<Arc<..Service>>`（`image_variants`、`emby_migration`）。
- 为什么不搬 `database`：`prepare_database`（`main.rs:51`）启动时总会打开数据库、`ready_with_proxy` 构造器固定
  `Some`，该 `Option` 服务的是 `AppState::default()`（4 处测试构造）与 fail-closed；`config_dir`/`restart_handle`
  在 setup 模式也要用。故三者归 `CoreState`，其 12 处与 1 处守卫不随阶段二删除。

### 方案 A（渐进）：`AppState { ready: Option<Arc<ReadyServices>>, .. }`

```rust
pub struct AppState {
    core: CoreState,                    // 16 个字段：13 必需 + database / config_dir / restart_handle
    ready: Option<Arc<ReadyServices>>,  // 46 个业务服务，均为非 Option
}
```

- 每个 handler 顶部保留**一次**守卫（`let services = require_ready!(state);`，与现宏形态相同，仍是裸 503），
  之后 46 个字段不再逐个检查：检查点从 308 处降到"handler 数量"。
- 优点：单 `Router` 结构不变，`app_with_state` 签名不变，可逐模块迁移；缺点：守卫仍在，未达到类型级保证。

### 方案 B（终局，推荐与 A 组合）：双状态类型 + 双 `Router`

```rust
pub struct SetupState { core: CoreState, database_setup: DatabaseSetupService, setup: SetupService, .. }
pub struct ReadyAppState { core: CoreState, /* 46 个服务字段，非 Option */ }
```

- `setup_router() -> Router<SetupState>` 只挂 `/api/v1/setup/*`、健康/就绪探针、静态资源、登录前必需端点；
  `ready_router() -> Router<ReadyAppState>` 挂业务、Emby、admin、播放等路由。
- 前置事实（2026-10-30 核对代码）：今天**没有**"setup 完成即重启"。`setup_complete`（`users.rs:601`）成功后
  返回 `201`；`request_restart()` 唯一调用点是 `setup_database_restart`（`users.rs:546`，数据库配置变更后）；
  `build_app_state`（`src/main.rs:172`）不管 setup 是否完成都构建完整 `AppState`，`main.rs:251` 的
  `app_with_state(app_state)` 无条件注册全部路由（`requires_database_selection` 只是状态上的 `bool`）。
- 因此"启动时一次性决定 Router 类型"**不成立**：方案 B 必须先补前置工作——(a) setup 完成后触发进程重启
  （复用 `RestartHandle`，前端补"正在重启"引导），或 (b) 进程内热切换（外层持有可替换的 `Router`）。

## 4. 迁移步骤（每步可独立回滚、每步跑 `cargo build --locked` + 窄测试）

1. **抽 `ReadyServices`**：把 46 个业务服务从 `AppState` 搬进 `ReadyServices`，`AppState` 只留 `core`（16 个）
   与 `Option<Arc<ReadyServices>>`；`require_service!` 内部改读 `services.<field>`（调用点不变）。——**已完成**。
2. **加 `require_ready!`**：每个 handler 顶部取一次 `Arc<ReadyServices>`，把 308 处收缩为一次守卫（可分批）。
   ——**已完成，但只迁移代表性文件（见 §6）**，其余文件留待后续。
3. **拆 Router**：新增 `SetupState`/`setup_router()` 与 `ReadyAppState`/`ready_router()`；须先落地 §3 前置工作。
4. **删守卫**：删除 308 处（此时约等于 handler 数的）守卫；118 处残余 let-else 里只删明确是"未就绪"的分支
   （59 处 `api_error` + 37 处裸 503），保留 18 处降级返回、5 处 NOT_FOUND / 1 处 NOT_IMPLEMENTED 与日志分支。
5. **收尾**：`database_selection_required` 随状态类型消失；4 处 `AppState::default()` 测试构造改用 `SetupState`。

## 5. 改动面与风险

| 风险 | 说明 | 缓解 |
| --- | --- | --- |
| `Router` 泛型扩散 | 每个 handler 签名与 `State<T>` 绑定，路由构造集中在 `routes.rs`/`users.rs`/`admin.rs`，改动会触及它们的返回类型 | 方案 A 先行；B 必须先做 §3 的前置工作（setup 完成后重启或热切换） |
| 测试构造点 | `..AppState::default()` 的局部构造、`app_with_state(AppState::default())` 若签名变化，`tests/**` 需同步（本阶段禁止改动） | 保留 `app_with_state` 兼容入口 |
| 共享部件归属 | `request_access_ip`、限流、SSE 预算等中间件与 worker/plugin 的 `Arc` 副本在两种模式下都要用，不能整体塞进 `ReadyServices` | 中间件与无状态件放 `CoreState`，服务统一走 `Arc<ReadyServices>`，不 clone 个体服务 |

结论：**先做方案 A（低风险、可分批），方案 B 作为目标形态**。B 的"启动时定型 Router"并不被现有行为支持：
必须先补"setup 完成后重启"或"进程内热切换"二者之一，这是它的前置成本与产品决策点，而非既有事实。

## 6. 步骤 1、2 的落地结果（2026-11-03）

### 步骤 1：`ReadyServices` 拆分（行为不变）

- `AppState { core: CoreState, ready: Option<Arc<ReadyServices>> }`；`CoreState` 16 个字段、`ReadyServices`
  46 个字段，与 §3 的划分一致。
- `ReadyServices` 里 **40 个字段是普通字段**（非 `Option`），因为 ready 构造器总会建出它们；**6 个字段仍是
  `Option`**：`image_writes`、`metadata_selection`、`downloads`、`strm_playback`、`webhooks`、
  `content_restriction_index_worker`。原因是它们本来就可能在三类路径下缺失（`ImageWriteService`/`DownloadService`
  构造失败、`StrmPlaybackResolver` 建客户端失败、Webhook 初始化失败、索引 worker 由启动后 `&mut self` 方法补上），
  缺失是"就绪但降级"，不是"未就绪"。把它们升成非 `Option` 会改变行为，与本任务硬约束冲突，故保留并在结构体上注明。
- **兼容层**（后续步骤删除）：`AppState` 为 46 个业务字段生成同名读访问器（`state.catalog()` 等），返回
  `Option<&Service>`，与拆分前的字段语义逐字相同（未就绪与可选槽都是 `None`）；`CoreState` 通过 `Deref`
  暴露，`state.database`/`state.resources` 等中间件读法一行未改。`require_service!` 的 310 处调用点因此
  **文本完全不变**，只有宏体改成 `$state.$field()`。
- 其余 let-else 与 `?` 访问点（22 个文件、253 个访问位置做纯机械改写）：`state.foo.as_ref()` → `state.foo()`，
  `state.foo.clone()` → `state.foo().cloned()`，移动语义（`state.emby_auth`、`state.database`）改成借用，
  返回的状态码、错误体、日志与早退顺序不变。
- 服务副本语义：所有状态克隆共享同一个 `Arc<ReadyServices>`（`cloning_the_state_shares_one_ready_services_instance`
  断言 `Arc::ptr_eq` + 字段地址相同 + `strong_count == 2`）；启动期 builder（`with_strm_playback_resolver`、
  `with_embedded_subtitle_executable`、`with_hls_executable`、`with_scraper`、`start_content_restriction_index_worker`）
  通过 `Arc::make_mut` 修改：`Arc` 独享时原地改，已共享时只复制一份**句柄结构**（每个服务的 `Clone` 都是
  指针级克隆，内部仍共享 `Arc`/数据库连接池 —— 拆分前每个请求克隆整个 `AppState` 时就是这种克隆），
  builder 只改自己那份，其他持有者看到的服务实例不变。语义与拆分前"克隆状态后重建个别服务"完全一致；
  这也是集成测试 `metadata_api::*`（`tmdb.map_or(state.clone(), |tmdb| state.with_scraper(..))`）
  继续通过的前提 —— 首发实现用 `Arc::get_mut` 时该路径会静默失效，已用 `make_mut` 修正。
  `with_hls_executable` 用 `WebPlaybackSessionService` 的 `Clone`（内部 HLS 进程表、槽位、硬件状态全是
  `Arc`）重建同一个实例，等价于拆分前的按值移动。
- 公开签名：`AppState::default()`（`ready: None`）、`app_with_state(AppState)`、4 个 `ready*` 构造器不变；
  `tests/**` 4 处构造点一行未改。`src` 内 4 处 `..AppState::default()` 测试字面量改为新增的
  `#[doc(hidden)] AppState::with_database(db)`（`database` 是 core 状态，不是服务）。

### 步骤 2：`require_ready!` 与代表性迁移

新增宏（`src/api/mod.rs`）：`let services = require_ready!(state);`，返回 `Arc<ReadyServices>` 的克隆
（只是引用计数 +1，服务实例仍是同一份），失败分支与 `require_service!` 一样是**裸 503**（空 body）。

迁移选择与结果：

| 文件 | handler | 守卫数（前 → 后） | 说明 |
| --- | --- | --- | --- |
| `src/api/legacy.rs` | `lux_search_people` / `lux_get_person_items` / `lux_get_person` / `lux_set_person_favorite` / `lux_update_person` | 14 → 8 | 每个 handler 的 `access`/`people`/`catalog`（共 2~3 次）收敛为一次；`database`（core）守卫保留 |
| `src/api/admin/admin_users.rs` | `admin_get_user_account_preferences` | 3 → 2 | admin 里唯一同时要 `access` + `catalog` 的 handler |

汇总：这 6 个 handler 的守卫从 17 处降到 10 处（6 次 `require_ready!` + 4 次 core `database`），
`require_service!` 调用点由 310 处降到 297 处。

- 守卫位置：`require_ready!` 放在该 handler **最早一个服务守卫**的位置，而不是字面第一行。就绪标志在请求
  处理期间不会改变，同一 handler 内所有服务守卫的结局相同，因此上提到最早守卫位置**保持状态码顺序**
  （鉴权 401/403、参数 400、404 仍然先于未就绪 503）。
- **`admin_webhooks.rs` 与 `admin_people.rs` 未迁移**：核对代码后发现两个文件都没有 `require_service!` 调用，
  而是手写 `let Some(x) = state.<field>() else { api_error(503, DatabaseUnavailable, "…尚未就绪") }`。
  它们的未就绪分支返回**带自定义中文消息的结构化 503**（例如"人物索引服务尚未就绪"），与 `require_ready!`
  的裸 503 不同（裸 503 会被 `normalize_empty_api_service_unavailable` 改写成"数据库暂时不可用"）。
  改成 `require_ready!` 会改变响应体，违反"对外行为逐字不变"，因此留给步骤 4 统一决定（步骤 4 本来就要
  删除这些"未就绪"分支）。同类 59 处 `api_error(DatabaseUnavailable)` 守卫都属此类。

### 剩余工作（步骤 3–5）

- 仍有 297 处 `require_service!` 与其余 let-else 未迁移（其中 109 处 `state.<field>() else {` 形态、
  90 处仍是 503 分支）；它们靠同名访问器编译，行为与拆分前一致。
- 步骤 3 的 Router 拆分仍受 §3 前置工作（setup 完成后重启或进程内热切换）约束，未变。
- 步骤 4/5 需要删掉访问器兼容层与 `Deref`、把 `database_selection_required` 交给状态类型，并把
  `src` 内的 `with_database` 测试辅助换成 `SetupState` 构造。
