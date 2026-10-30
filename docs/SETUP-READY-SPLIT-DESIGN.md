# 拆分 SetupState / ReadyAppState 与双 Router（方案 B 第 ③ 步设计）

日期：2026-11-05。状态：**设计待确认（本阶段只写文档，未改任何 `src/**`、`tests/**`、`web/**`）**。
范围：`src/api/**` 的路由树、状态类型与 handler 签名，以及 `src/main.rs` 的模式选择。
前置：方案 A 的步骤 1、2 已合并（`docs/APP-STATE-NEXT.md` §6）；方案 B 的第 ② 步（setup 完成后重启）
在 `tspace/setup-restart-backend` 分支上（设计见 `docs/SETUP-RESTART-DESIGN.md`）。

本文回答 `docs/APP-STATE-NEXT.md` 方案 B 步骤 3–5 的全部落地问题：泛型扩散、330 个 handler 的分批迁移、
setup 模式的路由集、`main` 的模式判定、后台 worker 的 `Arc` 语义、删守卫范围、回滚边界、风险与测试策略。
**不含实现**；阶段 2 由项目所有者确认后放行。

## 0. 一句话结论

| 问题 | 结论 | 详见 |
| --- | --- | --- |
| 1 泛型扩散 | **两棵独立路由树**，handler 与路由构造函数一律不加类型参数。只有两个受控泛型边界：`core_routes<S: FromRef<CoreState>>()` 与 `setup_routes<S: FromRef<SetupState>>()`（各 ≤8 条路由，因为同一组 setup handler 要在两棵树里各挂一次），加一个迁移期专用、约 15 行的 `StateView` trait（只服务 10 个跨文件鉴权 helper，最后一批删除）。20 层中间件全部"无状态或只读 `CoreState`"，抽成 `apply_common_layers(Router)` 两棵树各调用一次，**不重复写**。 | §2 |
| 2 handler 迁移 | 7 批，先小后大；过渡期用 **`impl FromRef<ReadyAppState> for AppState`** 让未迁移 handler 继续工作，因此每批只改自己文件里的 handler + 本地 helper，`cargo build`/集成测试在每批都绿，可独立回滚。首批一次性把 24 个 `api_routes()` 的返回类型从 `Router<AppState>` 改成 `Router<ReadyAppState>`（纯类型改动，无行为变化），此后每批都是文件内改动。 | §3 |
| 3 setup 模式路由 | 只挂 4 组：core 探针（`/health/live`、`/health/ready`、`/api/v1/version`）、setup 路由组（`/api/v1/setup/*` 6 条）、登录前最小端点（`/api/v1/auth/me`，固定 401）、静态资源 + SPA。Emby 只保留 2 条公共发现路由（`/System/Ping`、`/System/Info/Public`），其余 Emby 与全部 Lux 业务路由**不注册**：Lux 命名空间由现有中间件改写成 JSON 404，Emby 命名空间是裸 404，`/items/*` 这类不在命名空间表里的路径回落 SPA。与现状的差异逐条列出。 | §4 |
| 4 `main` 选模式 | 谓词是 `requires_database_selection \|\| !SetupService::status()?`（`status()` = `users.has_users()`，一条查询）。查询失败 **fail-closed 选 Ready**：setup 模式能创建首个管理员、改写数据库配置，是敏感方向；选了 Ready 而数据库真的坏，只是业务接口 503 + 进程按今天的路径退出，不会给出提权入口。`tests/**` 的 3 处 `AppState::default()`、8 处 `app()`、163 处 `AppState::ready(..)` 全部不改，靠 `AppState` 兼容门面 + `app_with_state` 分派保持语义。 | §5 |
| 5 后台 worker | 不变。worker 由 `AppState::start_*` 用**服务克隆**（`self.people()`、`self.scan_jobs().cloned()`）启动，从不持有 `AppState`；拆分后 `ReadyAppState`/`SetupState` 都只是同一份 `Arc<ReadyServices>` 的视图（`Arc::ptr_eq`），`Arc::make_mut` 仍只在构造期使用。 | §6 |
| 6 删守卫 | `require_service!` 301 处调用点 + 108 处 `state.<field>() else {}`：**删 388 处**（required 服务、`database`、setup 服务，ready 模式下不可达），**保留 19 处**（6 个可选服务的降级分支，响应体逐字不变）。`api_error(503, DatabaseUnavailable, "…尚未就绪")` 的 79 处结构化 503 全删，因为 `ready` 构造器总会建出这些服务，分支不可达。 | §7 |
| 7 回滚边界 | 8 个提交，前 7 个可单独回滚（每个都编译 + 集成测试绿）。第 8 个（删兼容层）与第 1 批合并后不可单独回退：一旦某 handler 改成 `State<ReadyAppState>`，回退它必须同时回退它的 `api_routes()`，属于同一提交。 | §8 |
| 8 风险 | 最大风险是"冻结测试 `tests/**` 钉住的 not-ready 契约"与"未就绪不注册业务路由"直接冲突；本节给出 6 条必须保留的兼容响应、被否决的方案与被否决原因。**有 3 个需要所有者确认的决策点**。 | §9 |
| 9 测试 | 新增 2 个集成测试组用例证明"setup 树里业务路径 404 / 探针 200"与"ready 树行为不变"；后者等价于"现有全量集成测试不改即通过"，逐组列出要跑的 target。 | §10 |

## 1. 现状基线（代码证据）

测量方法见附录 B，数字都可以用同一组命令复现（基线 `test` = `c4861069`）。

| 事实 | 证据 |
| --- | --- |
| 状态已分为 `CoreState`（16 个字段）与 `ReadyServices`（46 个字段，40 个非 `Option` + 6 个可选槽） | `src/api/legacy.rs:175`（`CoreState`）、`src/api/legacy.rs:202`（`ReadyServices`） |
| `AppState` 仍是"一个 `Router` + 一个状态"：`core: CoreState` + `ready: Option<Arc<ReadyServices>>` | `src/api/legacy.rs:259`（结构体）、`src/api/legacy.rs:307`（`ready_services`）、`src/api/legacy.rs:317`（`ready_services_mut`，`Arc::make_mut`） |
| 46 个同名读访问器仍是 `Option<&Service>`，`require_service!` 因此零文本改动 | `src/api/legacy.rs:321`（`required_service_accessor!`）、`src/api/legacy.rs:364`（`optional_service_accessor!`） |
| 路由集中在一处，中间件 20 层挂在同一个 `Router` 上 | `src/api/routes.rs:123`（`app_with_state`）、`src/api/routes.rs:133-311` |
| 路由表分 5 个入口：`users`(32)、`admin`(20 个文件)、`lux_api`(45)、`emby`(73)、`logo.svg` | `src/api/users.rs:3122`、`src/api/admin.rs:76`、`src/api/lux_api.rs:3`、`src/api/emby.rs:47`、`src/api/routes.rs:134` |
| Emby 树挂两次（裸路径 + `/emby` 嵌套），外面再套一层大小写归一化 | `src/api/routes.rs:138-139`、`src/api/routes.rs:311`、`src/api/routes.rs:359` |
| 带 `State<AppState>` 的 handler/extractor 共 **359 处**，分布在 **27 个文件** | 附录 B 命令 1 |
| 跨文件共享的鉴权 helper 都取 `&AppState`：`require_admin`、`require_web_user/principal/csrf`、`require_emby_user*`、`request_client_ip` 等 | `src/api/admin/admin_common.rs:3`、`src/api/users.rs:2789`、`src/api/users.rs:2834`、`src/api/emby_handlers.rs:2616`、`src/api/legacy.rs:1420` |
| 取 `&AppState` 的 helper 共 **111 个**：**86 个只被本文件调用**（随所在文件一起迁移），**25 个跨文件**；其中 `request_access_ip`/`request_client_ip`/`current_emby_server_name` 只读 `database`，可直接改成取 `&CoreState` 而与状态拆分解耦 | 附录 B 命令 2、`src/api/legacy.rs:1441`、`src/api/emby_handlers.rs:387` |
| `main` 不管 setup 是否完成都构建完整 `AppState`，并无条件挂全部路由 | `src/main.rs:172`（`build_app_state`）、`src/main.rs:187`（`AppState::ready_with_proxy`）、`src/main.rs:251`（`app_with_state`） |
| "setup 是否完成"的唯一权威信号是 `users.has_users()` | `src/application/setup.rs:17`（`status`）、`src/application/setup.rs:26`（`complete`） |
| `requires_database_selection` 只影响 2 个端点与 3 处 worker 启动，不是路由开关 | `src/main.rs:98`、`src/api/users.rs:456`、`src/api/users.rs:616`、`src/api/legacy.rs:918/935/954` |
| 中间件层只读 `CoreState`（限流器、`resources`、`request_metrics`），没有任何一层需要业务服务 | `src/api/routes.rs:153`（`limit_request_rate`）、`src/api/routes.rs:233`（home 延迟）、`src/api/routes.rs:290`（请求指标） |
| 冻结测试用 `app()` / `AppState::default()` 断言"未就绪时业务路由返回 503" | `tests/api_web/api_error_contract.rs:12,19,32,43`、`tests/api_web/health.rs:11`、`tests/api_web/ready_version.rs:14`、`tests/plugins/danmaku_api.rs:204`、`tests/api_web/web_static.rs:9,116,125` |

## 2. 问题 1：泛型扩散如何控制

结论：**两棵独立路由树，handler 一律绑定具体状态类型**；泛型只出现在 3 个受控位置（2 个路由构造函数 +
1 个迁移期 trait）；20 层中间件抽成一个函数，两棵树各调用一次。

### 2.1 目标类型形态

```rust
// 迁移期保留，终局也保留：它是 tests/** 唯一允许的构造入口（见 §5.4）
#[derive(Clone, Default)]
pub struct AppState {
    core: CoreState,
    database: Option<Database>,
    services: Option<Arc<ReadyServices>>,
}

/// Router<ReadyAppState> 的状态：46 个服务是非 Option 字段。
#[derive(Clone)]
pub struct ReadyAppState {
    core: CoreState,
    database: Database,
    services: Arc<ReadyServices>,
}

impl std::ops::Deref for ReadyAppState {          // core 字段读取零改动（110 处）
    type Target = CoreState;
    fn deref(&self) -> &CoreState { &self.core }
}

impl ReadyAppState {
    pub(crate) fn database(&self) -> &Database { &self.database }
    // 40 个 required 服务：与步骤 1 的访问器**同名**，但返回 &Service 而不是 Option<&Service>
    pub(crate) fn catalog(&self) -> &CatalogService { &self.services.catalog }
    // 6 个可选服务：保持 Option<&Service>，降级分支的响应体因此逐字不变（§7）
    pub(crate) fn webhooks(&self) -> Option<&WebhookService> { self.services.webhooks.as_ref() }
}

/// Router<SetupState> 的状态：只暴露 setup 需要的 4 个服务 + database + core。
/// 内部仍持有同一份 Arc<ReadyServices>（见 §2.4 的"为什么 setup 模式照样构建全部服务"），
/// 但**不实现 Deref 到 ReadyAppState**，也不暴露其余 42 个访问器，因此业务 handler
/// （State<ReadyAppState>）在类型层面挂不到这棵树上。
#[derive(Clone)]
pub struct SetupState(ReadyAppState);
impl std::ops::Deref for SetupState { type Target = CoreState; /* 同 ReadyAppState */ }
impl SetupState {
    pub(crate) fn setup(&self) -> &SetupService { &self.0.services.setup }
    pub(crate) fn database_setup(&self) -> &DatabaseSetupService { &self.0.services.database_setup }
    pub(crate) fn libraries(&self) -> &LibraryService { &self.0.services.libraries }
    pub(crate) fn scan_jobs(&self) -> &ScanJobService { &self.0.services.scan_jobs }
    // probe / metadata_reidentify / thumbnails 由 scan_worker_services() 需要，见 §4.3
}
```

要点：

- 迁移一批 handler 只需要改三样：`State<AppState>` → `State<ReadyAppState>`、删守卫、
  文件内 helper 的 `&AppState` → `&ReadyAppState`。因为步骤 1 已经把字段读取统一成
  `state.<field>()` 调用形态（`src/api/legacy.rs:321`、`:364`），**同名方法返回 `&Service` 让
  handler 主体几乎不用改**：`require_service!(state.catalog)` 只需变成 `let catalog = state.catalog();`。
- 全仓 `state.<field>()` 调用点 321 处，其中 `require_service!` 301 处、`state.X() else {` 108 处；
  其余约 40 处是 `is_some()/as_ref()/map()` 形态（`state.database.as_ref()` 占多数，属 core），
  在对应批次随守卫一起改写（§7 有分类清单）。
- `database` 的歧义必须处理：`CoreState.database` 是 `Option<Database>` **字段**，而 `ReadyAppState::database()`
  是返回 `&Database` 的**方法**。若保留字段读取，ready handler 里 `state.database` 与 `state.database()`
  语义不同，是实打实的坑。**建议在同一提交里把 `CoreState` 的字段改名为 `database_slot` 并提供
  `CoreState::database(&self) -> Option<&Database>`**，让"读数据库"永远是一个方法调用；
  涉及 36 处文本（附录 B 命令 5），机械且可编译校验。

### 2.2 为什么不用 trait 抽象整个状态

| 候选 | 结论 | 原因 |
| --- | --- | --- |
| `trait HttpState { fn catalog(&self) -> Option<&CatalogService>; }` + `State<impl HttpState>` | **否决** | handler 仍然拿到 `Option`，等于退回方案 A，达不到"ready 模式字段非 Option"。 |
| 一个 `trait` 提供 `fn catalog(&self) -> &CatalogService`，两棵树都实现 | **否决** | axum 的状态是 `Router<S>` 的类型参数，`State<T>` 提取靠 `S: FromRef<T>`；两个不同树共用一个 handler 必须让 handler 泛型化（330 个 handler + 24 个 `api_routes()` 全部变成泛型函数），扩散面正好是我们要消除的。 |
| 双 Router + 3 个受控泛型点 | **采纳** | 见 §2.3。 |

### 2.3 三个受控泛型点

1. `fn core_routes<S>() -> Router<S> where S: Clone + Send + Sync + 'static, S: FromRef<CoreState>`：
   `/health/live`、`/health/ready`、`/api/v1/version` 三条（`src/api/users.rs:3124-3126` 现有注册要搬到这里）。
   两棵树都要挂。
2. `fn setup_routes<S>() -> Router<S> where S: Clone + Send + Sync + 'static, S: FromRef<SetupState>`：
   `/api/v1/setup/*` 6 条（`src/api/users.rs:3135-3149`）。**两棵树都要挂**，因为
   (a) 前端每次启动都先调 `/api/v1/setup/status`（`web/src/app/App.tsx:65`、`web/src/lib/api/client.ts:395-399`），
   (b) `tests/storage/database_setup_api.rs:31` 在 **ready 树**里驱动 `/setup/database/*` 与 `/setup/complete`，
   并断言"setup 已完成后再调这些端点是 409"。
   这要求 `impl FromRef<ReadyAppState> for SetupState`（克隆 4 个服务句柄 + core，等价于今天每请求克隆整状态）。
3. 迁移期 `StateView`（§3.3），最后一批删除。

两个 `FromRef` 实现 + 一个 `Deref`（`ReadyAppState → CoreState`、`SetupState → CoreState`）就是全部
"状态之间"的黏合剂：

```rust
impl FromRef<SetupState> for CoreState
impl FromRef<ReadyAppState> for CoreState
impl FromRef<ReadyAppState> for SetupState
impl FromRef<ReadyAppState> for AppState   // 迁移期专用，§3.1
```

### 2.4 中间件：一份 layer 栈，两棵树共用

代码证据：`app_with_state` 的 20 层 `.layer()`（`src/api/routes.rs:147-310`）中，**没有任何一层需要业务服务**：

| layer | 依赖 | 证据 |
| --- | --- | --- |
| `json_response_etag` | 无 | `src/api/routes.rs:20` |
| `CompressionLayer` + `CompressJsonResponses` | 无 | `src/api/routes.rs:109`、`:149` |
| `limit_request_rate` | `CoreState.request_rate_limiter` | `src/api/routes.rs:153`、`:554` |
| catalog/home 信号量 | `app_with_state` 里新建的 `Arc<Semaphore>` | `src/api/routes.rs:129-131`、`:157` |
| `attach_peer_address` | 无 | `src/api/legacy.rs:1064` |
| `normalize_lux_api_key_query` | 无 | `src/api/admin/admin_common.rs:205` |
| 3 个 Emby trace layer + `reject_playback_token_outside_allow_list` + `reject_unmatched_emby_video_path` + `reject_unmatched_api_path` | 无（纯路径判断） | `src/api/legacy.rs:1131-1184`、`:1305`、`:1324`、`:1356` |
| `normalize_empty_api_service_unavailable` | 无 | `src/api/legacy.rs:1030` |
| home 延迟记录 | `CoreState.resources` | `src/api/routes.rs:233` |
| `strip_client_request_id` + `SetRequestId` + `TraceLayer` | 无 | `src/api/routes.rs:251-287` |
| `security_response_headers`、`web_asset_cache_policy` | 无 | `src/api/routes.rs:580`、`:611` |
| 请求指标 | `CoreState.request_metrics` | `src/api/routes.rs:290` |

因此把这一整段原样抽成

```rust
fn apply_common_layers(router: Router, core: &CoreState, slots: RequestSlots) -> Router
```

两棵树各调用一次即可，**不重复写层、也不会漏层**。`Router::layer` 保持调用顺序就等于保持今天的包裹顺序
（最后一个 `.layer()` 最外层），重构时把 20 次调用按出现顺序搬进函数体，不改顺序，语义逐字不变。

### 2.5 `app_with_state` 重构草图

```rust
pub(super) fn app_with_state(state: AppState) -> Router {
    match state.into_mode() {
        AppMode::Ready(ready) => ready_router(ready),
        AppMode::NotReady(core) => not_ready_compat_router(core),   // 只服务冻结测试，见 §5.4/§9.2
    }
}

fn ready_router(state: ReadyAppState) -> Router {
    let core = state.core().clone();
    let slots = RequestSlots::default();
    let (emby_routes, emby_path_templates) = emby::api_routes();   // 返回 Router<ReadyAppState>
    let router = Router::new()
        .route("/logo.svg", get(web_logo))
        .merge(core_routes::<ReadyAppState>())
        .merge(setup_routes::<ReadyAppState>())
        .merge(users::api_routes())
        .merge(admin::api_routes())
        .merge(lux_api::api_routes())
        .merge(emby_routes.clone())
        .nest("/emby", emby_routes)
        .fallback_service(spa_service())
        .with_state(state);                                        // -> Router<()>
    let router = apply_common_layers(router, &core, slots);
    case_insensitive_emby_router(router, emby_path_templates)       // 今天的最外层，保持最后一个
}

pub(super) fn setup_router(state: SetupState) -> Router {
    let core = state.core().clone();
    let slots = RequestSlots::default();
    let router = Router::new()
        .route("/logo.svg", get(web_logo))
        .merge(core_routes::<SetupState>())
        .merge(setup_routes::<SetupState>())
        .merge(setup_pre_login_routes::<SetupState>())              // /api/v1/auth/me -> 401
        .merge(setup_emby_discovery_routes::<SetupState>())         // 2 条公共发现（§4.2）
        .fallback_service(spa_service())
        .with_state(state);
    let router = apply_common_layers(router, &core, slots);
    case_insensitive_emby_router(
        router,
        vec!["/System/Ping".to_owned(), "/System/Info/Public".to_owned()],
    )
}
```

三个不可省的细节：

1. `emby_routes` 今天被 `merge` 一次 + `nest("/emby")` 一次（`src/api/routes.rs:138-139`），且
   `Router::nest` 会消耗 router，所以必须 `clone()` 后各用一次——与今天完全一致。
2. `case_insensitive_emby_router`（`src/api/routes.rs:359`）用 `fallback_service` 包住整棵树，
   必须在所有 layer 之后应用；它接受 `Router`（`Router<()>`），因此两棵树都能复用，只需换路径模板表。
3. `SPA fallback` 必须两棵树都有：setup 页面本身是 SPA 资源（`web/src/app/App.tsx:89` 依据
   `setup.data.initialized` 渲染 `SetupPage`），没有静态资源就没有 setup 界面。

## 3. 问题 2：330 个 handler 的迁移策略

可分批、可独立回滚，且每批都编译通过。实测 handler/extractor 共 **359 处**（27 个文件），
`require_service!` 调用点 **301 处**，`state.<field>() else {}` 守卫 **108 处**（附录 B 命令 1–3）。

### 3.1 过渡期"两种 State 并存"：只保留一个方向的转换

```rust
/// 迁移期专用：未迁移的 handler 仍取 State<AppState>，在 ready 树上由它降级得到。
impl FromRef<ReadyAppState> for AppState {
    fn from_ref(state: &ReadyAppState) -> AppState {
        AppState {
            core: state.core.clone(),
            database: Some(state.database.clone()),
            services: Some(Arc::clone(&state.services)),   // 同一份 Arc，服务实例不变
        }
    }
}
```

| 方向 | 是否实现 | 理由 |
| --- | --- | --- |
| `FromRef<ReadyAppState> for AppState` | **实现（迁移期）** | 未迁移 handler 在 ready 树上照常工作；`Arc::ptr_eq` 保证服务实例不分裂。最后一批后删除。 |
| `FromRef<AppState> for ReadyAppState` | **不实现** | `AppState` 可能是未就绪的（`AppState::default()`），转换只能返回 `Option`/panic。让它在运行期 panic 或发明"半就绪"状态都违背本步骤的目的。 |
| `Deref<Target = AppState> for ReadyAppState` | **不实现** | 会把整张 Option 访问器表又暴露一遍，`state.catalog()` 的解析重新变成歧义（方法优先解析到哪一层取决于定义顺序），迁移进度变得不可见。 |
| `Deref<Target = CoreState>`（两个新类型各一个） | **实现** | `state.remote_access`/`state.server_id`/`state.config_dir` 等 110 处 core 字段读取零文本改动。 |

`AppState` 的兼容访问器（`src/api/legacy.rs:321-376`）保留到批 9，只被 `require_service!`、
`require_ready!` 与尚未迁移的文件使用。迁移进度＝"这些访问器的调用点数"，可量化。

### 3.2 一批迁移的标准动作（每批一个提交）

1. 该文件内 `State(state): State<AppState>` → `State<ReadyAppState>`（函数签名一行）。
2. `let x = require_service!(state.x);` → `let x = state.x();`（返回 `&Service`）。
3. 删除 required 服务的 `let Some(x) = state.x() else { …503… };` → `let x = state.x();`
   （**保留** 6 个可选服务的降级分支，§7）。
4. 文件私有 helper 的 `state: &AppState` → `state: &ReadyAppState`（86 个里属于该文件的那部分）。
5. 改写该文件里 `state.<field>()` 的 Option 惯用法（`?`、`is_some()`、`as_ref()`、`map()`）；
   可选服务不在此列。
6. `api_routes()` 的返回类型**不用动**（批 0 已统一改成 `Router<ReadyAppState>`）。
7. `cargo build --locked` + 该组窄集成测试 + `cargo clippy --locked --all-targets --all-features -- -D warnings`。

因为文件之间只剩 `StateView`（§3.3）这一条通道，**任何一批都可以单独回滚**：
回滚后该文件回到 `State<AppState>` + 守卫，其它文件不受影响，`FromRef` shim 仍然让它编译。

### 3.3 跨文件 helper：`StateView` 过渡 trait

111 个取 `&AppState` 的 helper 里 25 个被别的文件调用（附录 B 命令 2），它们是最主要的耦合来源：
`require_admin`、`require_web_user/principal/csrf`、`require_emby_user/principal*`、
`record_audit_event`、`spawn_library_scan`、`load_lux_item_detail`、`serve_media_file`、
`admit_emby_direct_playback` 等。为了让批次不被这些文件绑在一起，迁移期给它们一个 15 行的 trait：

```rust
pub(crate) trait StateView: std::ops::Deref<Target = CoreState> {
    fn ready_services(&self) -> Option<&ReadyServices>;
}
impl StateView for AppState { fn ready_services(&self) -> Option<&ReadyServices> { self.services.as_deref() } }
impl StateView for ReadyAppState { fn ready_services(&self) -> Option<&ReadyServices> { Some(&self.services) } }

// 25 个跨文件 helper 的签名：
pub(crate) async fn require_admin(
    headers: &HeaderMap,
    state: &impl StateView,     // 静态分发，无虚调用开销
    require_csrf: bool,
) -> Result<(), Response>
```

- `impl Trait` 在参数位置是隐式泛型，调用点文本不变（`require_admin(&headers, &state, true)`），
  迁移前后都能编译。
- 批 9 删除 trait，把 25 个 helper 收敛成 `&ReadyAppState`（此时已无 `AppState` 调用者）。
- 三个只读 `database` 的 helper（`request_access_ip` `src/api/legacy.rs:1441`、`request_client_ip`
  `:1420`、`current_emby_server_name` `src/api/emby_handlers.rs:387`）**不进 trait**，批 0 直接改成取
  `&CoreState`：它们本来就不需要服务，改完就是终局形态，少一次迁移。

### 3.4 批次划分（先小后大，每批 handler 数已核对）

| 批次 | 文件 | handler | `require_service!` | 状态 `else` 守卫 | 主要验证 |
| --- | --- | --- | --- | --- | --- |
| **批 0** | `routes.rs` + `legacy.rs`（新类型、`FromRef`、`StateView`、`apply_common_layers`、`api_routes` 返回类型、`into_mode`/兼容树） | 0 | 0 | 0 | `cargo build` + `--test api_web` + `--test storage database_setup_api::` |
| **批 1** | `admin_dashboard`(1) `admin_health`(1) `admin_performance`(1) `admin_database_diagnostics`(3) | 6 | 4 | 0 | `--test admin admin_health::` 等 4 组 |
| **批 2** | `admin_scheduled_tasks`(10) `admin_logs_events`(5) `admin_items`(6) | 21 | 15 | 2 | `--test admin`、`--test library`、`--test metadata` 相关模块 |
| **批 3** | `admin_transcoding`(3) `admin_user_invitations`(4) `admin_user_registrations`(3) `admin_webhooks`(9) | 19 | 11 | 10 | `--test api_web webhooks_api::`、`--test auth_users` |
| **批 4** | `admin_settings`(14) `admin_plugins`(13) `admin_emby_migration`(11) `admin_people`(9) | 47 | 18 | 16 | `--test admin admin_settings::`、`--test plugins`、`--test metadata people_api::` |
| **批 5** | `admin_jobs`(22) `admin_metadata`(17) `admin_libraries`(13) | 52 | 33 | 16 | `--test admin`、`--test library libraries_api::`、`--test scanner` |
| **批 6** | `admin_users`(47) | 47 | 48 | 1 | `--test admin admin_users::`、`--test auth_users` |
| **批 7** | `emby_resume_visibility`(1) `emby_catalog`(25) `emby_handlers`(35) | 61 | 53 | 16 | `--test emby`（10 个模块） |
| **批 8** | `playback`(26) `media`(37) `legacy`(7) | 70 | 96 | 21 | `--test playback`、`--test images`、`--test api_web web_playback::` |
| **批 9** | `users`(36，其中 6 个 setup handler 走 `State<SetupState>`)、`admin_common`(2 处守卫)、`mod.rs`（删两个宏 + 单测）、`legacy.rs`（删 46 个兼容访问器与 `FromRef`）、`StateView` | 36 | 23 | 26 | `--test auth_users`、`--test api_web web_auth::`、`--test storage database_setup_api::` |

合计 359 个 handler / 301 处 `require_service!` / 108 处状态守卫，与附录 B 的实测一致。
`users.rs` 放在最后，因为它的 6 个 setup handler 要同时切到 `State<SetupState>`（`setup_status`、
`setup_database_*`、`setup_complete`），而这些 handler 也挂在 ready 树上（§2.3）。

排序原则：单文件规模从小到大；`admin_users` 与 `users` 两个巨型文件各自独立成批；Emby 三件套
（`emby_catalog`/`emby_handlers`/`playback`/`media` 互相调用）放在中后段，此时 `StateView` 已覆盖
所有跨文件 helper，批次边界与文件边界一致。

### 3.5 批 0 的具体内容（一次性、纯类型，无行为变化）

1. 新增 `ReadyAppState`/`SetupState`/`AppMode` 与 3 个 `FromRef`（含迁移期 `AppState` 那个）。
2. `AppState::into_mode()`：`(Some(db), Some(services))` → `Ready`，否则 `NotReady(core)`。
3. 24 个 `api_routes()` 的返回类型 `Router<AppState>` → `Router<ReadyAppState>`（`src/api/users.rs:3122`、
   `src/api/admin.rs:76` 起的 19 个 `*_routes`、`src/api/lux_api.rs:3`、`src/api/emby.rs:47` 的
   `EmbyRouteBuilder` 两处）。此时所有 handler 仍是 `State<AppState>`，靠 `FromRef` 解析 → 行为不变。
4. `routes.rs` 拆成 `ready_router` + `not_ready_compat_router` + `apply_common_layers`，
   `app_with_state` 变成分派器。**这一步就要把冻结测试的 6 条兼容响应补齐**（§4.5、§9.2），
   否则 `--test api_web`、`--test plugins danmaku_api::`、`--test api_web web_static::` 会红。
5. 把 `/health/live`、`/health/ready`、`/api/v1/version` 三条注册从 `users::api_routes()` 搬进
   `core_routes()`（纯搬移，路径与 handler 不变）。

批 0 之后，`main` 与 `tests/**` 一行都不用改；此后每批只碰自己的文件。
