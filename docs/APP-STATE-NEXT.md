# AppState 就绪状态类型化（第二阶段设计提案）

日期：2026-10-30。状态：**仅设计，未实施**。范围：`src/api/**`。第一阶段（纯机械收敛）已完成，
本文件只给出把"就绪/未就绪"从 `Option` 升级为类型状态的方案、改动面、迁移步骤与风险。

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
   与 `Option<Arc<ReadyServices>>`；`require_service!` 内部改读 `services.<field>`（调用点不变）。
2. **加 `require_ready!`**：每个 handler 顶部取一次 `Arc<ReadyServices>`，把 308 处收缩为一次守卫（可分批）。
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
