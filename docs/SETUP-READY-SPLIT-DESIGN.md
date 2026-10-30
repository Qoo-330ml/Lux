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

## 4. 问题 3：setup 模式下挂哪些路由

### 4.1 原则

1. **只挂"没有管理员也能正确回答"的路由。** 判据是可执行的事实：handler 是否需要 `auth`/`emby_auth`/
   业务服务来给出**正确**答案。
2. setup 模式的前端契约由代码决定，不靠猜：`web/src/app/App.tsx:65-89` 在渲染任何页面之前只调
   `/api/v1/setup/status` 与 `/api/v1/auth/me`；`SetupPage` 调 `/api/v1/setup/database`（`SetupPage.tsx:28`）；
   `DatabaseSetupPanel` 调 `/api/v1/setup/database/{test,select}`（`DatabaseSetupPanel.tsx:47,52`）；
   `AdminSetupForm` 调 `/api/v1/setup/complete`（`AdminSetupForm.tsx:23`）。
3. setup 树与"未就绪兼容树"是两棵不同的树，目的不同（§4.5），不要合并。

### 4.2 setup 树的路由清单

| # | 方法/路径 | handler | 依赖 | 为什么必须在 setup 模式可用 | 证据 |
| --- | --- | --- | --- | --- | --- |
| 1 | `GET /health/live` | `users::live`（`State` 都不用） | 无 | 容器/探针存活判定 | `src/api/users.rs:33` |
| 2 | `GET /health/ready` | `users::ready` | `CoreState.database`+`config_dir` | 就绪探针；setup 模式下数据库与配置目录正常时**今天也是 200** | `src/api/users.rs:152` |
| 3 | `GET /api/v1/version` | `users::version` | `CoreState.database` | 版本展示/排障 | `src/api/users.rs:205` |
| 4 | `GET /api/v1/setup/status` | `users::setup_status` | `SetupService` | SPA 启动第一个请求，决定渲染 SetupPage | `src/api/users.rs:229`、`web/src/app/App.tsx:65` |
| 5 | `GET /api/v1/setup/database` | `users::setup_database_status` | `DatabaseSetupService` | 数据库选择面板 | `src/api/users.rs:246` |
| 6 | `POST /api/v1/setup/database/test` | `users::setup_database_test` | `SetupService`+`DatabaseSetupService` | 数据库连通性测试 | `src/api/users.rs:332` |
| 7 | `POST /api/v1/setup/database/select` | `users::setup_database_select` | 同上 | 选择后端并落 `database.json` | `src/api/users.rs:390` |
| 8 | `POST /api/v1/setup/database/restart` | `users::setup_database_restart` | 同上 + `restart_handle` | 选完库后排重启 | `src/api/users.rs:452` |
| 9 | `POST /api/v1/setup/complete` | `users::setup_complete` | `SetupService` + `libraries` + 扫描子集（见 §4.3）+ `restart_handle`/`setup_restart_delay`（第 ② 步新增） | 创建首个管理员、可选取库与首次扫描、排重启 | `src/api/users.rs:601` |
| 10 | `GET /api/v1/auth/me` | **setup 专用**：固定 `401 AUTHENTICATION_REQUIRED "需要登录"` | 无 | SPA 并行探测会话；setup 模式不可能存在会话 | `web/src/app/App.tsx:69`，今天由 `src/api/users.rs:2935-2941` 给出同一响应 |
| 11 | `GET /System/Ping`、`POST /System/Ping` | 复用 `emby_ping` | 无 | Emby 客户端握手 | `src/api/emby_handlers.rs:842` |
| 12 | `GET /System/Info/Public` | 改为 `State<SetupState>`（两棵树共用一份） | `SetupService` + `CoreState.database`/`server_id` | 今天在 setup 模式返回 `{"StartupWizardCompleted": false}`，是 Emby 世界的"向导未完成"约定 | `src/api/emby_handlers.rs:397-410` |
| 13 | `GET /Users/Public` | **setup 专用**：固定 `200 []` | 无 | 今天 setup 模式的返回就是 `[]`（`emby_auth.public_users()` 在空用户表上给出空集），保持一致 | `src/api/emby_handlers.rs:850-870` |
| 14 | `GET /logo.svg` + 静态资源 + SPA fallback | `routes::web_logo`、`ServeDir` | 无 | setup 页面本身是 SPA | `src/api/routes.rs:134`、`:140-145` |

共 **13 条业务语义路由**（1–13）+ 静态/SPA。其中只有 #12 是"签名改造后两树共用"，
#10、#13 是两个 5~10 行的 setup 专用处理函数（无服务、无状态、响应体与今天逐字相同）。

setup handler 需要的服务**只有 4 个**：`SetupService`、`DatabaseSetupService`、`LibraryService`、
`ScanJobService`，外加 `probe`/`metadata_reidentify`/`thumbnails` 供 `spawn_library_scan` 组
`ScanWorkerServices`（`src/api/admin/admin_jobs.rs:3-11`、`:242-253`）。`SetupState` 暴露这 7 个就够，
其余 39 个服务在类型上不可达（§2.1）。

### 4.3 setup 模式下是否构建全部服务：**构建，行为不变**

`src/main.rs:187-206` 今天不管 setup 是否完成都会 `AppState::ready_with_proxy(...)` 并启动
7 类 worker。第 ③ 步**保持这一点**：`SetupState` 由同一个 `ReadyServices` 派生（只暴露子集），
`main` 仍调用同样的 builders 与 starters。

原因：`ScanJobService` 的构造是链式接线（`src/api/legacy.rs:606-630`：`home`、`library_covers`、
`strm_probe`、`people`、`local_nfo`、`metadata_selection`、`metadata_reidentify`、`webhooks`）。
在 setup 模式只建一个"半接线"的 `ScanJobService` 会让 setup 的首次扫描在扫完后的封面/人物/首页
读模型刷新上行为不同——那是**用户可见的行为变化**，违反"ready 模式行为不变"的验收口径。
把它留给后续步骤（把 setup 模式真正瘦身）单独评估。

推论：setup 模式下 worker 照旧运行（计划任务、实时监控、webhook 投递等），第 ③ 步不改变这一点；
`docs/SETUP-RESTART-DESIGN.md` §7 的回滚边界因此仍然成立。

### 4.4 setup 模式下不注册的路由，与现状对比

| 路径族 | 今天（setup 未完成） | 第 ③ 步之后 | 用户可见变化 |
| --- | --- | --- | --- |
| `/api/v1/{libraries,items,home,search,favorites,people,continue-watching,…}` | 401 `需要登录`（无会话；服务齐全，鉴权先失败） | 不注册 → SPA fallback 命中 `reject_unmatched_api_path`（`src/api/legacy.rs:1356-1389`）→ **JSON 404 `NOT_FOUND`** | 是。但前端在 `initialized=false` 时只渲染 `SetupPage`（`web/src/app/App.tsx:89`），无业务调用；第三方脚本会看到 401→404 |
| `/api/v1/admin/*` | 401/403 | JSON 404 | 同上；setup 向导不调 admin 接口 |
| `/api/v1/auth/{login,logout,sessions,profile,…}` | 401/400（无管理员时登录失败） | JSON 404 | 是；setup 页面不登录（`LoginPage` 只在 `initialized=true` 后渲染） |
| `/api/v1/auth/login-background` | 200 + 图片 | JSON 404 | 是；只被 `LoginPage` 使用（`LoginPage.tsx:16`），setup 模式不渲染 |
| Emby `/System/Info`、`/Items/*`、`/Users/{id}/…` 等 | 401/403/200（服务齐全） | 不注册 → Emby 命名空间是**裸 404**（`reject_unmatched_api_path` 对非 `/api/` 前缀不加错误信封，`src/api/legacy.rs:1376-1379`） | 是（401→404）。Emby 客户端只会显示"服务器不可用" |
| Emby `/System/Ping`、`/System/Info/Public`、`/Users/Public` | 200 | **200，响应逐字相同**（§4.2 #11-13） | 否 |
| `/items/{id}`（小写，不在 `is_api_or_emby_namespace_path` 名单里，`src/api/legacy.rs:1332-1354`） | Emby handler → 401/404 | SPA fallback → **HTML 200**（`Accept: text/html` 的详情页跳转因此仍可用） | 是（仅 JSON 请求会从 401/404 变成 HTML 200） |

关于"404 还是 503 带说明"的取舍：

- **不选 503**：503 会与"数据库暂时不可用"的既有语义混淆（`normalize_empty_api_service_unavailable`
  会把裸 503 改写成 `DATABASE_UNAVAILABLE`），而且 setup 模式的语义是"这些接口在这个进程里不存在"，
  404 才是准确的。更重要的是：**`/api/v1/no-such-endpoint` 必须继续 404**（`tests/api_web/api_error_contract.rs:230-236`
  钉住了这一条），所以"整个 `/api/v1/` 前缀一律 503"这条路本来就不通。
- **Emby 侧保持裸 404**：与今天"未注册的 Emby 路径一律裸 404"一致（`src/api/legacy.rs:1324-1330`、
  `tests/api_web/api_error_contract.rs:220-228`），不需要新的状态码。
- 想给运维看"正在 setup"的提示，用 `/health/ready`（始终可用）与前端提示，不要占用 API 状态码。

### 4.5 "setup 树"与"未就绪兼容树"的区别（不要混淆）

| | setup 树 | 未就绪兼容树 |
| --- | --- | --- |
| 谁构造 | `main`：`setup_router(SetupState)` | `app_with_state(AppState::default())` / `app()`，**只有 `tests/**` 会走到**（`src/main.rs` 从不构造 `AppState::default()`） |
| 状态 | `SetupState`（有 database、有 7 个服务） | `CoreState`（database 为 `None`，无服务） |
| 路由 | §4.2 的 13 条 + 静态 | 6 条冻结测试钉住的兼容响应 + 静态（§9.2） |
| 语义 | 真实的"等待初始化"模式 | 仅用于保持 `tests/**` 一行不改的历史契约 |

## 5. 问题 4：`main` 如何选择模式

### 5.1 判定谓词

```rust
enum AppMode { Setup, Ready }

// src/main.rs，prepare_database 之后、build_app_state 之前
async fn decide_mode(
    database: &Database,
    requires_database_selection: bool,
) -> AppMode {
    if requires_database_selection {
        return AppMode::Setup;                       // 数据库后端都还没选，必然是 setup
    }
    match SetupService::new(database.clone()).and_then(|setup| Ok(setup)) {
        Ok(setup) => match setup.status().await {
            Ok(true) => AppMode::Ready,
            Ok(false) => AppMode::Setup,
            Err(error) => {
                // fail-closed：见 §5.2
                error!(%error, "could not decide the startup mode; assuming the ready mode");
                AppMode::Ready
            }
        },
        Err(error) => { error!(%error, "…"); AppMode::Ready }
    }
}
```

判定只用一条查询：`SetupService::status()` = `UserStore::has_users()`（`src/application/setup.rs:17-19`）。
选它的理由：

- 它是 **`/api/v1/setup/status` 与 `setup_complete` 的同一判据**（`src/api/users.rs:237`），
  不存在"启动判定"与"接口判定"两套真源。
- 它跑在**已经连接并迁移完成的同一个 `Database`** 上（`src/main.rs:28` 的 `prepare_database` 已做完
  schema migration，`:64` 才读 schema version），所以不存在"表还没建好"的时序问题。
- 它不依赖配置文件：`database.json` 丢失但库里已有用户的场景会正确进入 ready（而不是把 setup
  能力重新递给 LAN 上的任何人）。

### 5.2 fail-closed 选哪一边：**选 Ready**

| 失败方向 | 选 Setup 的后果 | 选 Ready 的后果 |
| --- | --- | --- |
| 数据库瞬时不可用（迁移刚成功、`has_users()` 查询失败） | 进程把"创建首个管理员"的能力暴露给局域网（`setup_complete` → 409 之外还可能 5xx），并允许改写 `database.json`。**敏感能力被放大** | 业务路由返回 503（`normalize_empty_api_service_unavailable` 给结构化错误），管理员修复数据库后重启即可 |
| 数据库已初始化但状态查询失败 | 同上，且此时 `setup/complete` 会 409，界面表现为"setup 永远失败" | 一切照旧 |
| 数据库真坏 | setup 树也给不出正确答案（`setup_status` 自己会 503） | 与今天一致：`build_app_state` 失败 → 进程退出 → 由 supervisor/容器重启 |

结论：**"setup 能力"是权限更敏感的一侧，fail-closed = 不把它交出去 = 选 Ready**。
代码上表现为 `Err(_) => AppMode::Ready`，并用 `error!` 记录（不是 `warn!`：这会让一个已初始化的
服务器在本次启动里丢掉 setup 端点，属可观测事件）。

### 5.3 与 `requires_database_selection` / `prepare_database` 的关系

- `requires_database_selection`（`src/main.rs:98-99`）= "既没有显式 `database.json`，也没有历史 SQLite 库"，
  在这两种情况下 `prepare_database` 会先 `mark_database_selection_pending()`（`src/main.rs:54-56`）
  再连默认 SQLite。这是**配置层事实**，与 setup 是否完成无关，所以它是判定里的短路条件。
- 第 ③ 步**不动** `prepare_database`；只把 `requires_database_selection` 从 `CoreState` 移到
  `SetupState`（它是 setup 树的属性），`CoreState.database_selection_required`
  （`src/api/legacy.rs:179`）在第 ④ 步删除，配套 3 处 worker 短路
  （`src/api/legacy.rs:918/935/954`）改成"只有 ready 模式才启动这些 worker"。
  为保持 `tests/storage/database_setup_api.rs:31` 的 `.require_database_selection()` 可用，
  `AppState` 兼容门面继续保留这个方法名，转发到内部标志。

### 5.4 `AppState` / `app_with_state` 如何为 `tests/**` 保持兼容

冻结构造点实测（附录 B 命令 4）：

| 用法 | 处数 | 位置 | 第 ③ 步如何保持 |
| --- | --- | --- | --- |
| `AppState::default()` | 3 | `tests/api_web/web_static.rs:116`、`tests/plugins/danmaku_api.rs:204`、`tests/shutdown.rs:161` | `Default` 仍是"无 database、无服务"，语义不变；前两处经 `app_with_state` 落到**未就绪兼容树**（§9.2），第三处只调 `shutdown_background_workers_checked()`，与路由无关 |
| `app()` | 8 | `tests/api_web/{health,ready_version,web_static,api_error_contract}.rs` | `app()` 定义不变（`src/api/legacy.rs:972-974`）；`app_with_state` 内部按 `into_mode()` 分派 |
| `app_with_state(AppState::ready(..)…)` | 163 | 12 个测试文件 | `AppState::ready*` 构造器与全部 `#[doc(hidden)]`/公开 builder 方法保留；`app_with_state` 一律落到 `ready_router` |
| builder 方法 | `with_scraper`、`with_hls_executable`、`with_strm_playback_resolver`、`with_embedded_subtitle_executable`、`with_restart_handle`、`with_request_rate_limit`、`with_sse_connection_limit`、`require_database_selection`、`start_local_metadata_{outbox_worker,refresh}`、`start_scheduled_tasks`、`rebuild_people_index`、`rebuild_person_credit_index`、`resume_interrupted_metadata_jobs`、`shutdown_background_workers{,_checked}` | 分布在 `tests/**` | 全部保留在 `AppState` 上，内部改为操作 `services: Option<Arc<ReadyServices>>`；**不移动到 `ReadyAppState`**，否则冻结测试不能编译 |

`app_with_state` 的签名保持 `fn app_with_state(state: AppState) -> Router`（`src/api/legacy.rs:976`），
内部改为分派：

```rust
impl AppState {
    pub(crate) fn into_mode(self) -> AppMode {          // 迁移完成后直接返回 ready/setup 之一
        match (self.database, self.services) {
            (Some(database), Some(services)) => AppMode::Ready(ReadyAppState {
                core: self.core, database, services,
            }),
            _ => AppMode::NotReady(self.core),
        }
    }
}
```

因此**`tests/**` 的一行都不需要改**（本任务与后续实现均禁止修改）。唯一无法避免的是 §9.2 列出的
"未就绪兼容树"必须自己合成 6 条响应——那是 `src` 里 ~40 行、只被测试触达的代码，不是测试改动。

### 5.5 `main` 的重构草图

构造部分**完全不动**（`src/main.rs:172-207` 的 `build_app_state` 保持原样，包括 7 类 worker 启动），
只把"选路由树"插在构造与 `serve` 之间：

```rust
// src/main.rs
let (database, requires_database_selection) = prepare_database(&config).await?;
…
let mode = decide_mode(&database, requires_database_selection).await;   // §5.1
let app_state = build_app_state(&config, &database, control_tx.clone()).await?;   // 不变
let shutdown_state = app_state.clone();
let app = match mode {
    // setup 模式与今天一样构建全部服务、启动全部 worker（§4.3），只是挂 setup 树
    AppMode::Setup => setup_router(SetupState::from(app_state.into_ready()?)),
    AppMode::Ready => ready_router(app_state.into_ready()?),
};
serve(config, discovery_config, database, app, shutdown_state, control_tx, control_rx).await
```

- `into_ready() -> Result<ReadyAppState, StartupError>`：`services` 缺失时返回启动错误（不 panic、不
  `unwrap`；`main` 已经把 `AppState::ready_with_proxy` 调过，缺失就是内部不变量被破坏，按启动错误处理）。
- `serve` 只增加一个参数：把 `app_state: AppState` 换成 `app: Router` + `shutdown_state: AppState`，
  内部其余逻辑（`src/main.rs:253-286`：graceful shutdown、worker drain、`database.close()`、
  `restart_process()`）一字不改。
- 第 ② 步的 `resume_shutdown_interrupted_first_scans`（分支 `tspace/setup-restart-backend`，
  `AppState::resume_shutdown_interrupted_first_scans`）留在 `build_app_state` 里不动：它自带
  `database_selection_required` 短路，且"被关停打断的首次扫描"意味着上一次运行已完成 setup。
**优点**：`main` 的改动面只有 6 行；setup 模式的资源/worker 行为与今天逐字相同，回滚第 ③ 步只需把
`match` 换回 `app_with_state(app_state)`。

## 6. 问题 5：后台 worker / 插件持有的服务副本

结论：**拆分不改变任何 `Arc` 共享语义**，因为没有一个后台任务持有 `AppState`——它们持有的是服务句柄，
而服务句柄本来就通过 `Arc`/数据库连接池共享（`docs/APP-STATE-NEXT.md` §6 已写明这条前提）。

证据与拆分后的对应关系：

| 事实 | 证据 | 拆分后 |
| --- | --- | --- |
| worker 启动方法只取服务克隆 | `src/api/legacy.rs` 的 `rebuild_people_index`（`self.people()`）、`start_realtime_watchers`（`self.core.database.clone()` + `self.scan_jobs().cloned()` + `self.libraries()`）、`start_webhook_worker`/`start_scheduled_tasks`/`start_database_diagnostics` 同形 | 这些方法留在 `AppState` 上（`main` 仍先构造 `AppState`，§5.5），行为不变；终局可以原样搬到 `ReadyAppState` |
| 全仓没有任何 `AppState` 被 `tokio::spawn` 捕获后长期持有 | `grep -rn "spawn" src/api/legacy.rs | …`：`resume_interrupted_metadata_scraping` 内部捕获的是服务；`src/main.rs:227` 的 scanner 捕获 `Database` | 无变化 |
| 服务实例的唯一所有者是 `Arc<ReadyServices>`；克隆状态只增加引用计数 | `src/api/legacy.rs:2490-2507` 的 `cloning_the_state_shares_one_ready_services_instance`（`Arc::ptr_eq` + 字段地址相同 + `strong_count == 2`） | `ReadyAppState`/`SetupState` 持有**同一个** `Arc`：`AppState::into_ready()` 与 `impl FromRef<ReadyAppState> for AppState` 都只 `Arc::clone`，不改指向 |
| `Arc::make_mut` 只在构造期改服务（会写 `Option` 槽与 scaler 等） | `src/api/legacy.rs:317-319`（`ready_services_mut`）以及 5 个 builder：`with_strm_playback_resolver`、`with_embedded_subtitle_executable`、`with_hls_executable`、`with_scraper`、`start_content_restriction_index_worker` | 保留在 `AppState` 上（`tests/**` 需要在 `app_with_state` 之前用它们），且**规定 `into_ready()` 之后不得再调用**：它是消费型转换，类型上已经拦住（`ReadyAppState` 没有 builder 方法） |
| 插件只拿到 `PluginService` 句柄 | `src/api/legacy.rs` 的 `ReadyServices.plugins`，被 `danmaku`/`strm_probe`/`chapter_detection`/`scheduled_tasks` 等克隆 | 同上：句柄克隆，内部状态在 `Arc` |

唯一需要额外守住的纪律（写进实现注释与 review 清单）：

1. `ReadyAppState`/`SetupState` 的构造必须只经由 `AppState::into_ready()`（或它派生的
   `SetupState::from`），不得直接 `Arc::new(ReadyServices { .. })`。
2. `into_ready()` 之后任何 `Arc::make_mut` 都会在"已被共享"的情况下复制一份句柄结构，
   使某些 worker 看到旧句柄——与拆分前的错误用法等价，因此 `into_ready` 之后不再提供可变入口。
3. 每请求克隆（`FromRef` 与 `State<T>` 提取）只做 `CoreState`/`Database`/`Arc` 的克隆，
   与今天"每请求克隆整个 `AppState`"同阶或更便宜。

## 7. 问题 6：删守卫（第 ④ 步）的范围

### 7.1 实测分类

| 类别 | 处数 | 第 ④ 步处置 | 理由 |
| --- | --- | --- | --- |
| `require_service!(state.<required>)` | 173 | **删** | ready 模式下 `ReadyServices.<field>` 是非 `Option` 字段，分支不可达 |
| `state.<required>() else { … }`（结构化 503 / 裸 503 / 降级返回） | 84 | **删** | 同上；包括 `admin_people.rs`（12 处、"人物索引服务尚未就绪"）与 `admin_handlers` 里返回 `false`/`NOT_FOUND` 的降级分支 |
| `require_service!(state.database)` | 121 | **删** | `ReadyAppState::database()` 返回 `&Database`（`into_ready()` 只在 `database` 为 `Some` 时成功） |
| `state.database() else { … }` | 2 | **不删（搬走）** | `users.rs:153`/`:206`（`/health/ready`、`/api/v1/version`）随批 0 搬进 `core_routes`，仍是 `State<CoreState>` 上的 Option 守卫：探针必须能在数据库缺失时回答 503，它不属于 ready 树的代码 |
| `state.setup()` / `state.database_setup()` 守卫 | 5 + 5 | **删** | `SetupState` 里两者都是非 `Option` 字段 |
| `require_service!` on 6 个可选服务（`image_writes` 3、`downloads` 2、`strm_playback` 2） | 7 | **保留** | `ReadyServices` 里它们是 `Option`（`src/api/legacy.rs:202-256` 的注释：缺失是"就绪但降级"，不是"未就绪"） |
| `state.<optional>() else { … }`（`webhooks` 10、`metadata_selection` 2） | 12 | **保留，响应体逐字不变** | 同上：`webhooks` 初始化失败、`metadata_selection` 未配置都是合法降级 |
| 其余 Option 惯用法（`state.<field>()?`、`.is_some()`、`.map()`、`.as_ref()`） | ~40 | 按服务类别分别处理 | required 服务改成直接绑定；可选服务不变 |

合计：**删 388 处**（173+84+121+10），**保留 19 处**（7+12）外加可选服务的 ~10 处非守卫惯用法。
两处 `database` 守卫（`users.rs:153`/`:206`）留在 `core_routes` 里，属于"未就绪探针必须能回答 503"的语义，
不随 ready 树删除。

### 7.2 为什么"结构化 503"也能删（且不改响应体）

`docs/APP-STATE-NEXT.md` §6 曾把"人物索引服务尚未就绪""Webhook 通知服务尚未就绪"这类**带自定义中文
消息的结构化 503** 列为"不能改响应体"，因此步骤 2 没有迁移它们。第 ④ 步能删的依据是：

- 这些消息来自 `ReadyServices` 的 **required** 槽（`people`、`catalog`、`access`、`library_covers`…），
  而 `ReadyServices` 是**一次构造、全有或全无**（`src/api/legacy.rs:471` 起的
  `ready_with_proxy_and_image_config` 单函数建出全部字段，缺一个就整体启动失败）。
  因此"ready 模式下 `people` 缺失"这个状态**不存在**，分支不可达，删除不改变任何可达响应。
- 真正可达的"缺失"只有 6 个 `Option` 槽，它们的响应体（例如 `admin_webhooks.rs:92` 的
  "Webhook 通知服务尚未就绪"）**保留不动**。
- 冻结测试里唯一触及"服务缺失"的是 `AppState::default()`（无服务），它走的是未就绪兼容树（§9.2），
  不经过这些 handler。

### 7.3 必须保留的清单（实现时逐条核对）

```
src/api/admin/admin_webhooks.rs      10 处  state.webhooks() else { …结构化 503 "Webhook 通知服务尚未就绪" }
src/api/media.rs                      2 处  state.metadata_selection() else { …降级 503 }
src/api/…                            7 处  require_service!(state.{image_writes,downloads,strm_playback})
src/api/media.rs                   ~3 处  state.image_writes()/downloads()/strm_playback() 的 is_some/map 惯用法
src/api/users.rs:153,206              2 处  core 探针的 database 守卫（随 core_routes 留下）
```

### 7.4 第 ④ 步与批 1–9 的关系

删守卫**不是**一个独立提交：批 1–9 每批在改签名时顺手删掉该文件的守卫（否则 `state.catalog()` 返回
`&CatalogService` 后旧守卫根本编译不过）。因此 `docs/APP-STATE-NEXT.md` 的"步骤 4"在本设计里被并入
步骤 3 的批次；真正的收尾提交（批 9）只负责删除**兼容层**：46 个 Option 访问器、`Deref<Target = CoreState>`
for `AppState`、`FromRef<ReadyAppState> for AppState`、`StateView`、`require_service!`/`require_ready!`
两个宏及其 6 个单元测试（`src/api/legacy.rs:2416-2485` 区域）。

## 8. 问题 7：回滚边界与提交序列

### 8.1 提交序列

| # | 提交 | 内容 | 可单独回滚 | 回滚后仍编译 |
| --- | --- | --- | --- | --- |
| C0 | 本设计文档 | 只加 `docs/SETUP-READY-SPLIT-DESIGN.md` | 是 | 是（不动代码） |
| C1 | **批 0**：类型与树骨架 | `ReadyAppState`/`SetupState`/`AppMode`、3 个 `FromRef`（含迁移期 `AppState`）、`StateView`、`apply_common_layers`、24 个 `api_routes()` 返回类型、`app_with_state` 分派 + 未就绪兼容树、`core_routes` 搬移 | 是 | 是（回到今天的单树 `AppState`，§8.3 有例外说明） |
| C2–C9 | **批 1–8**：文件组迁移 | 每批改 `State<AppState>` → `State<ReadyAppState>`、删守卫、文件私有 helper 改签名 | 是，逐批独立 | 是（`FromRef<ReadyAppState> for AppState` 让未迁移文件继续工作） |
| C10 | **批 9**：收尾 | `users.rs` + 6 个 setup handler 切 `State<SetupState>`、`admin_common` 2 处守卫、删两个宏与 6 个宏单测、删 46 个 Option 访问器、删 `FromRef<ReadyAppState> for AppState`、删 `StateView`、`main` 插入 `decide_mode` + 选树 | **否**（见 §8.2） | — |
| C11 | 观察期后的清理（可选） | `CoreState.database_selection_required` 移入 `SetupState`，3 处 worker 短路改写 | 是 | 是 |

`main` 的 6 行改动（§5.5）放进 C10 而不是 C1：C1 之后 `app_with_state` 已经能按状态分派，
`main` 继续用 `app_with_state(app_state)` 也完全正确（永远走 ready 树），因此**在批次迁移期间不需要
动 `main`**；只有 setup 树真正可用（C10 完成 setup handler 的 `State<SetupState>` 切换）时，才把
`main` 切到显式选树。

### 8.2 不可单独回退的组合

1. **C10 必须整体回滚。** 它同时删除 `FromRef<ReadyAppState> for AppState`、`StateView` 与 46 个
   Option 访问器；只回退其中一半会出现"某文件仍在用兼容访问器但访问器已删"或"某 helper 仍取
   `&impl StateView` 但 trait 已删"的编译错误。回滚方式：`git revert C10` 单个提交即可（它在历史里
   是原子的），不要把 C10 拆成多提交。
2. **批 i 的 handler 改动与它的守卫删除是同一个提交。** 改签名后旧守卫（`require_service!` 返回
   `Option<&Service>` 形态）必然编译不过，中间态不可存在。
3. **`api_routes()` 的返回类型翻转（C1）不能单独回退而保留任何批 1–8 的成果。** 回退 C1 意味
   所有 `api_routes()` 回到 `Router<AppState>`，此时 `State<ReadyAppState>` 的 handler 无法解析
   `FromRef<AppState> for ReadyAppState`（本设计明确不实现，§3.1）→ 编译失败。因此回退顺序必须是
   **先回退 C2–C10（或整体回到 C0），再回退 C1**。

### 8.3 C1 单独回滚的一个已知例外

C1 把 `/health/live`、`/health/ready`、`/api/v1/version` 三条注册从 `users::api_routes()` 搬进
`core_routes()`，并在 `app_with_state` 里引入未就绪兼容树。回退 C1 会同时撤掉兼容树——这在
C1 之后、C2 之前是安全的（此时还没有 handler 迁移）。但**如果 C2–C9 已经落地**，回退 C1 会破坏
§8.2 第 3 条的约束；所以"回退 C1"只在"刚提交 C1 就发现构建/测试红"这个窗口内成立。

### 8.4 每个提交的验证口径

| 提交 | 必跑 |
| --- | --- |
| C0 | `cargo fmt --all -- --check`（只碰 docs，可省） |
| C1 | `cargo build --locked`；`cargo test --locked --test api_web`；`cargo test --locked --test storage database_setup_api::`；`cargo test --locked --test plugins danmaku_api::` |
| C2–C9 | `cargo build --locked` + 批 1–8 表里该组的窄命令（`cargo test --locked --test <group> <module>::`，见 `docs/TEST-TARGETS.md`）+ `cargo clippy --locked --all-targets --all-features -- -D warnings` |
| C10 | 全量门禁：`cargo build --locked`、`cargo test --locked --all-targets`、`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings`、`python3 scripts/check-migration-parity.py`、`python3 scripts/check-backend-branches.py` |
| C11 | 同 C10 的全量门禁（它动 `CoreState` 字段与 worker 启动条件） |

不 push、不合并到其它分支；C10 之前的每个提交都可以单独 `git revert`。
