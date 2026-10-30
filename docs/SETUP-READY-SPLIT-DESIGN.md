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
