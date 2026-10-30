# setup 完成后重启进程（方案 B 第 ② 步设计）

日期：2026-11-04。状态：**设计待确认（尚未改任何 `src/**`、`web/**`）**。范围：`setup_complete`
成功路径的重启触发、首次扫描的存续、前端“正在重启”引导，以及对应的测试与回滚策略。

本文件是 `docs/APP-STATE-NEXT.md` 方案 B 的**第 ② 步**：先让“setup 完成后重启进程”成为既有行为，
第 ③ 步（拆 `SetupState`/`ReadyAppState` 与 `setup_router()`/`ready_router()`）才成立。本文档不含
Router 拆分。

## 0. 一句话结论

| 问题 | 结论 | 详见 |
| --- | --- | --- |
| 1 重启时机 | 响应在 handler 里同步构造并返回；`spawn` 一个 250 ms 后 `request_restart()` 的任务。响应**不会丢**是因为 `axum::serve` 的 graceful shutdown 会排空在途请求（`src/main.rs:253-284`），延迟只为避开“写首字节”竞争。延迟可通过 `with_setup_restart_delay` 注入，测试用 200 ms 断言顺序 | §2 |
| 2 首次扫描 | 推荐 (b)：setup **照旧**启动首次扫描，新增启动恢复流程“为被关停打断的首次扫描重新排队”。谓词：`RECONCILE_LIBRARY` + `CANCELLED` + `error='SERVER_SHUTDOWN'` + `last_scan_at IS NULL` + 启用 + 有 root + 无活动任务 | §3 |
| 3 幂等与失败 | setup 成功即不可回退：无句柄→201 + `restarting:false`；`request_restart()` 返回 `false`→视为“已排上重启”；重复/并发调用→409 且不排期；排期点在管理员创建成功之后（含部分成功的 4xx） | §4 |
| 4 前端 | `setup()` 返回体加可选 `restarting`；`AdminSetupForm` 交棒给 `SetupPage` 的全屏“正在重启 Lux”（复用数据库流程样式）；就绪判据用“先断开再恢复”，不能只看 `initialized` | §5 |
| 5 测试 | 进程内注入 `watch` 句柄断言“先 201 再 Restart”；`exec` 只在 bin 目标，测试天然不触发；恢复流程用例放 `tests/scanner/shutdown_jobs.rs` | §6 |
| 6 回滚 | 三个代码提交互相独立、无 schema 变更、不碰 `ReadyServices`/`require_ready!`/路由；回滚边界在第 ③ 步之前 | §7 |

## 1. 现状基线（代码证据）

| 事实 | 证据 |
| --- | --- |
| `setup_complete` 在**一个请求里**完成：建管理员 → 可选取库 → 可选加根 → 启动首次扫描 → 返回 201 | `src/api/users.rs:601`（入口）、`src/api/users.rs:729`（`spawn_library_scan`）、`src/api/users.rs:739`（`scanJob` 字段）、`src/api/users.rs:751`（`StatusCode::CREATED`） |
| 重启机制是**一次性**的 `watch` 通道 | `src/application/restart.rs:10-27`（`RestartHandle::request_restart` 用 `send_if_modified` 只在 `Running` 时生效，第二次返回 `false`；`restart.rs:41-42` 的单元测试已经断言“只能请求一次”） |
| 现成的重启先例：数据库切换 | `src/api/users.rs:452`（`setup_database_restart`）、`src/api/users.rs:546`（`request_restart()`）、`src/api/users.rs:555`（返回 `202` + `{"restarting": true}`） |
| 重启句柄只由 `main` 注入 | `src/main.rs:195`（`.with_restart_handle(RestartHandle::new(control_tx))`）、`src/api/legacy.rs:752`（`with_restart_handle`）。集成测试构造的 `AppState`（如 `tests/api_web/web_auth.rs:24`）**没有**重启句柄 |
| 真正的 `exec` 只发生在 bin 目标 | `src/main.rs:284`（`restart_process()?`）、`src/main.rs:346`/`src/main.rs:357`（`#[cfg(unix)]` 用 `Command::exec`，非 unix 返回错误）。`restart_process` 是 `main.rs` 私有函数，库目标与集成测试都不可达 |
| 关停顺序：先停 HTTP（优雅排空）→ 停后台 worker → 取消未完成任务 → 关库 → `exec` | `src/main.rs:253`（`axum::serve`）、`src/main.rs:257`（`with_graceful_shutdown`）、`src/main.rs:268`（`shutdown_background_workers_checked`）、`src/main.rs:271`（`cancel_incomplete_jobs_for_shutdown`）、`src/main.rs:281`（`database.close()`）、`src/main.rs:284`（`restart_process`） |
| 关停/启动都会把未完成任务标为 `CANCELLED` + `error='SERVER_SHUTDOWN'` | `src/storage/jobs/shutdown.rs:1`（常量）、`src/storage/jobs/shutdown.rs:13`（单事务批量取消，含 `scan_jobs` 的 `PENDING`/`RUNNING`）、`src/main.rs:161`（启动时也调用一次） |
| 项目已有“恢复被关停打断的任务”的先例 | `src/api/legacy.rs:953`（`resume_interrupted_metadata_scraping`，由 `src/main.rs:202` 调用）、`src/application/reidentify.rs:2466`（`resume_interrupted_metadata_jobs`）、`src/storage/jobs/metadata_reidentify/lifecycle.rs:561`（`list_shutdown_interrupted_metadata_reidentify_job_ids`：判据就是 `status='CANCELLED' AND error='SERVER_SHUTDOWN'`） |
| 扫描任务**没有**对应的恢复流程 | `src/api/legacy.rs:869`（`start_scheduled_tasks` 只同步计划任务与读模型，不含“首次扫描”）；全仓 `start_library_scan`/`spawn_library_scan` 的调用点只有 `src/api/users.rs:729`（setup）与 `src/api/admin/admin_libraries.rs:1163`（管理员手动触发） |
| “库是否扫描过”有持久信号：`libraries.last_scan_at` | 建表 `migrations/0005_libraries.sql`；写入点 `src/storage/library_roots.rs:257`（`update_library_last_scan`），调用点 `src/application/scanner/jobs/manifest_index.rs:1399`、`reconciliation.rs:1192`、`incremental.rs:100`。**没有历史回填**（全仓仅这 4 处出现该列） |
| 创建扫描任务时会拒绝重复/停用/丢库 | `src/application/scanner/jobs/lifecycle.rs:877`（入口）、`lifecycle.rs:900`（停用库 → `LibraryNotFound`）、`lifecycle.rs:903-909`（已有活动任务 → `AlreadyActive`）、`lifecycle.rs:910`（读取 roots） |

推论（本文档全部方案的前提）：

1. 只要 201 已经写出，setup 就是**已完成的事实**，重启机制的任何失败都不能改写响应语义。
2. `RestartHandle` 一次性 ⇒ 并发/重复调用只会有一个真正排上重启；后续调用拿到 `false`，语义是
   “已经在准备重启”，不是错误。
3. 首次扫描的**意图已经落库**（`spawn_library_scan` 先建 `scan_jobs` 行再 `tokio::spawn` worker，
   见 `src/application/admin_scan_jobs.rs:147-157`），所以重启后“要不要扫”不需要新的标记表。

## 2. 设计问题 1：重启时机（201 送出之后）

**目标**：客户端必须先拿到 `201` + 管理员/库信息，进程才进入关停。

**做法**（handler 末尾；“管理员已创建”的所有返回路径，见 §4.2）：

1. 构造完响应 JSON（含 `restarting` 字段，见 §4），克隆 `state.restart_handle`。
2. `tokio::spawn` 一个延迟任务：`sleep(delay)` → `handle.request_restart()`；失败只记 `info` 日志
   （一次性句柄，说明已有其它调用排在前面）。
3. **同步返回** 201，不等待该任务。

```rust
// src/api/users.rs，handler 末尾（管理员已创建的所有返回路径，见 §4.2）
let mut response = /* ... 已有的 201 / 4xx 响应体与状态码 ... */;
if admin_created {
    if let Some(handle) = state.restart_handle.clone() {  // core 状态字段，见 src/api/legacy.rs:180
        let delay = state.setup_restart_delay();          // 新增：默认 250 ms，测试可注入
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if !handle.request_restart() {
                tracing::info!("restart was already requested before setup completed");
            }
        });
        response["restarting"] = json!(true);             // 仅 201 路径会带这个字段
    } else {
        tracing::warn!("setup completed without a restart handle");
        response["restarting"] = json!(false);            // 嵌入式/测试状态，见 §4.3
    }
}
```

**为什么“响应一定先送出”（不依赖延迟大小）**：

- 重启请求只是把 `watch` 置为 `ShutdownReason::Restart`；`src/main.rs:257` 的 `with_graceful_shutdown`
  收到后**停止接受新连接并排空在途请求**，`axum::serve(...).await`（`src/main.rs:253`）返回时该响应
  已经写完。
- `exec` 在 `serve` 返回之后还要经过 `shutdown_background_workers_checked`（`src/main.rs:268`）、
  `cancel_incomplete_jobs_for_shutdown`（`src/main.rs:271`，一次事务）、`database.close()`
  （`src/main.rs:281`）才执行（`src/main.rs:284`）。这几步本身也给了数毫秒到数十毫秒的余量。
- 因此 250 ms 延迟的作用只有两个：给 hyper 把响应写进 socket 的机会（避免“watch 变更”和“写首字节”
  竞争），以及避免 `setup_complete` 在被取消的任务里做任何收尾。**它不是正确性保证**。

**为什么不用“响应 body 完成后再触发”的精确信号**（`http-body = "1"` 已是直接依赖，
`Cargo.toml:25`，技术上可写一个 `Body` 包装器在 `poll_frame` 返回 `Ready(None)`/`Drop` 时发 `oneshot`）：

- 它能证明“hyper 已经读完了这个 body”，但不能证明“客户端收到了”，对用户可见结果没有更强保证；
- 需要额外约 40 行 `http_body::Body` 转发样板（`size_hint`/`is_end_stream`/`poll_frame` 三条都要代理），
  而收益被 graceful shutdown 的排空语义覆盖；
- 保留为**可选加固**：若阶段 2 的集成测试发现 201 偶发丢失，再补该包装器，接口（`restarting` 字段、
  注入延迟）不变。

**延迟可注入**：`setup_restart_delay` 放在 `CoreState`（`src/api/legacy.rs:175` 起的结构体，
与 `restart_handle` 同处），默认 `Duration::from_millis(250)`，并提供
`#[doc(hidden)] pub fn with_setup_restart_delay(mut self, delay: Duration) -> Self`
（与现有 `AppState::with_database` 的 `#[doc(hidden)]` 测试辅助同风格）。测试用 200 ms 断言顺序、
用 0 ms 断言“最终会请求”。

**测试里如何避免真的 `exec`**：不需要任何开关。`exec` 只在 `src/main.rs:284` 调用，属于 bin 目标；
集成测试走 `app_with_state(...)`（例：`tests/api_web/web_auth.rs:24-33`），进程内只有 `watch` 通道。
只要阶段 2 不新增“测试里跑二进制”的用例，就不会触发重启。另外 `tests/shutdown.rs:59` 已经会
spawn 真实二进制，但只发 SIGTERM（`ShutdownReason::Shutdown`），与本文档无关；若将来要端到端验证
重启，必须在该组里显式加 `LUX_*` 开关。

## 3. 设计问题 2：首次扫描会被重启取消

### 3.1 问题与证据链

1. setup 的首次扫描是一次真实的 `RECONCILE_LIBRARY` 任务：`spawn_library_scan`（`src/api/admin/admin_jobs.rs:242`）
   → `start_library_scan`（`src/application/admin_scan_jobs.rs:143`）**先建任务行、再 `tokio::spawn`
   worker**（`src/application/admin_scan_jobs.rs:147-157`）。
2. 进程关停时 `cancel_incomplete_jobs_for_shutdown` 会把 `PENDING`/`RUNNING` 的 `scan_jobs` 统一改成
   `CANCELLED`、`error='SERVER_SHUTDOWN'`（`src/storage/jobs/shutdown.rs:1`、`:46-56`），新进程启动时又取消一次
   （`src/main.rs:161`）。
3. 启动流程里**没有任何**“为未扫描过的库启动扫描”的步骤：`start_scheduled_tasks`（`src/api/legacy.rs:869`）
   只同步计划任务、读模型、清理 worker；`resume_interrupted_metadata_scraping`（`src/api/legacy.rs:953`）
   只恢复元数据刮削。全仓 `start_library_scan` 的调用点只有 setup（`src/api/users.rs:729`）与管理员手动触发
   （`src/api/admin/admin_libraries.rs:1163`）。
4. 结论：**若不做任何事**，setup 创建的第一个库在重启后停在“有 roots、零媒体项、`last_scan_at IS NULL`、
   一条被取消的扫描任务”的状态，直到管理员自己去点“扫描”。

补充事实：`libraries.last_scan_at` 是“扫描已成功跑完”的现成持久信号（建表
`migrations/0005_libraries.sql`，写入 `src/storage/library_roots.rs:257`，调用点
`src/application/scanner/jobs/manifest_index.rs:1399`、`reconciliation.rs:1192`、`incremental.rs:100`），
且**没有历史回填**（全仓只有这 4 处出现该列名）。

### 3.2 候选方案

| 编号 | 机制 | 是否改动 setup 的扫描行为 | 是否新增持久化 | 主要代价 |
| --- | --- | --- | --- | --- |
| **(b) 推荐** | setup **继续**启动首次扫描；启动流程新增“恢复被关停打断的首次扫描” | 否 | 否（复用 `scan_jobs` 行作为意图） | 任务历史里会留下一条 `CANCELLED/SERVER_SHUTDOWN` 记录；策略上“任何被重启打断的首次扫描都会被重试” |
| (a1) | setup **不再**启动扫描；重启后按 `lux_meta` 标记启动 | 是（201 的 `scanJob` 变 `null`） | 是（新 storage API + `lux_meta` 键） | 两处真源；`lux_meta` 目前只在 `src/storage/database_cleanup.rs:384-401` 内部使用，需要新开公共 API；标记的清理/重试语义要另写 |
| (a2) | setup 不再启动扫描；重启后对 `last_scan_at IS NULL` 的库启动扫描 | 是 | 否 | **旧库会被批量重扫**：`last_scan_at` 属于 manifest 时代才有写入、无回填，升级后第一次启动会给所有历史库排队做全量扫描 |
| (c) | 保留 setup 扫描，但关停时**豁免**它（新列/新状态，或“刚创建的库不取消”） | 否 | 是（迁移） | 破坏 `src/storage/jobs/shutdown.rs:9-12` 明确写下的“单事务取消、启动与关停不留混合集合”不变量；扫描续跑还要处理半成品 manifest |
| (d) | 前端在服务就绪后调用现有 `POST /api/v1/admin/libraries/{id}/scan` | 否 | 否 | 不构成保证：关标签页、反向代理抖动、管理员还没登录都会漏；只能当兜底 UX |
| (e) | setup 请求里等扫描跑完再返回 | 是 | 否 | 违反 `AGENTS.md`“不要在用户请求里扫描整个库”，201 会悬挂数分钟 |
| (f) | 把重启延迟拉长到扫描完成 | 否 | 否 | 同上，等于把一次全量扫描塞进 setup 的等待窗口 |

### 3.3 推荐方案 (b) 的具体形态

**setup 不动扫描行为**（`src/api/users.rs:729` 保持原样），新增一个启动恢复步骤：

1. **storage**：在 `src/storage/jobs/scan/lifecycle.rs`（或 `src/storage/jobs/shutdown.rs` 旁边）加
   `list_shutdown_interrupted_first_scan_library_ids(after: Option<&str>, limit: i64)`，判据与
   `list_shutdown_interrupted_metadata_reidentify_job_ids`（`src/storage/jobs/metadata_reidentify/lifecycle.rs:561`）
   同构，复用项目已有的“`CANCELLED` + `SERVER_SHUTDOWN` ⇒ 可恢复”约定：

   ```sql
   SELECT jobs.library_id
   FROM scan_jobs jobs
   JOIN libraries lib ON lib.id = jobs.library_id
   WHERE jobs.job_type = 'RECONCILE_LIBRARY'
     AND jobs.status = 'CANCELLED'
     AND jobs.error = 'SERVER_SHUTDOWN'
     AND lib.is_enabled = 1
     AND lib.last_scan_at IS NULL
     AND EXISTS (SELECT 1 FROM library_roots root WHERE root.library_id = lib.id)
     AND NOT EXISTS (
         SELECT 1 FROM scan_jobs active
         WHERE active.library_id = jobs.library_id AND active.status IN ('PENDING', 'RUNNING'))
     AND (? IS NULL OR jobs.library_id > ?)
   GROUP BY jobs.library_id
   ORDER BY jobs.library_id
   LIMIT ?
   ```

   `GROUP BY` 解决“连续重启留下多条取消记录”的重复；`NOT EXISTS` 用现成的活动任务判据
   （`src/storage/jobs/scan/lifecycle.rs:235-247`）。

2. **application**：在 `src/application/admin_scan_jobs.rs`（`start_library_scan` 旁，`:143`）加
   `resume_shutdown_interrupted_first_scans(scan_jobs, workers, limit)`：逐库调用现成的
   `start_library_scan`，把 `AlreadyActive`（`src/application/scanner/jobs/lifecycle.rs:903-909`）与
   `LibraryNotFound`（停用/已删，`:900`）当“跳过”，其它错误 `tracing::warn!` 后继续；单次启动最多处理
   32 个库，避免病态数据造成扫描风暴。

3. **启动接线**：在 `src/api/legacy.rs` 的 `resume_interrupted_metadata_scraping`（`:953`）旁加
   `AppState::resume_shutdown_interrupted_first_scans()`，同样以 `tokio::spawn` 后台执行、同样在
   `database_selection_required`（`:954`）与 `scan_jobs` 缺失时直接返回；在 `src/main.rs:202`
   之后调用一行。

### 3.4 如何保证“setup 创建的第一个库在重启后仍会被扫描一次”

| 环节 | 保证 | 证据 |
| --- | --- | --- |
| setup 请求内任务已落库 | 是（`start_library_scan` 先 `create_*_job` 后 `spawn`，201 返回时行已存在） | `src/application/admin_scan_jobs.rs:147-157` |
| 关停把“意图”转成可恢复状态 | 是（`PENDING` 或 `RUNNING` 都改成 `CANCELLED/SERVER_SHUTDOWN`） | `src/storage/jobs/shutdown.rs:46-56` |
| 断电/`SIGKILL` 场景 | 是（残留 `RUNNING` 会在下次启动被同样标记，谓词同样命中） | `src/main.rs:161` |
| 重启后恢复 | 是（谓词命中 → 新任务 + 新 manifest） | §3.3 |
| 不重复扫描 | 是（扫描若在重启前已跑完，`last_scan_at` 已写，谓词不命中） | `src/storage/library_roots.rs:257` |
| **遗漏面**：setup 时扫描**没能启动** | 否 | `src/api/users.rs:729-735` 只 `warn`；此时无任务行、无证据，201 里 `scanJob` 为 `null`。用户可在管理页手动扫描。若必须覆盖，改用 (a1) 的 `lux_meta` 标记 |

### 3.5 为什么不用“复用被取消的任务行”续跑

扫描的续跑依赖 manifest 检查点（`scan_manifests` / `scan_manifest_roots`），被关停打断的任务可能处于
“manifest 部分写入”的中间态；`cancel_incomplete_jobs_for_shutdown` 里对 `RECONCILE_LIBRARY` 还专门
保留了一条“没有 manifest 才允许按旧版重扫”的分支（`src/storage/jobs/shutdown.rs:18-38`）。对**首次**
扫描而言，新建任务 + 新 manifest 与“从头重扫”等价且语义简单，所以恢复流程创建**新任务**，并在任务
历史上留下“取消 → 新任务完成”的清晰链路。

## 4. 设计问题 3：幂等与失败

### 4.1 原则

**setup 一旦成功就不回退**：管理员已经写入数据库（`src/auth/users.rs:617-638`），重启机制的任何失败都只能
降级为“本次不重启”，不能把 201 改成错误。

### 4.2 重启请求的“排期点”：管理员创建成功之后

推荐把排期放在 `setup.complete(..)` 返回 `Ok` 之后（`src/api/users.rs:692-694` 的 `match` 成功分支），而不是只在 201 分支末尾：

- 因为后面的步骤（校验路径、建库、加根、起扫描）都可能返回 4xx；如果只在 201 分支排期，就会出现
  “管理员已存在、setup 已永久关闭，但进程仍停在 setup 模式”的半状态。方案 B 第 ③ 步之后，这个半状态
  意味着 ready 路由压根没挂载，管理员无法自助建库，只能重启进程。
- 排在“handler 末尾”而不是紧跟 `complete()` 之后，可以避免一次网络路径上的 `inspect_root_path`
  与重启延迟重叠（延迟从 handler 结束开始算）。

结论：handler 里记一个 `admin_created: bool`，在返回响应前统一排期；**4xx 响应体不加 `restarting` 字段**
（保持 `api_error` 的既有形状），前端把它当普通错误展示，重启后 `setup/status` 变 `initialized: true`，
页面自然进入登录。

### 4.3 三种失败/重入场景

| 场景 | 行为 | 理由 |
| --- | --- | --- |
| `state.restart_handle` 为 `None` | 返回 201，`restarting: false`，`warn!` 一条日志 | 生产路径一定注入句柄（`src/main.rs:195`）；`None` 只出现在库内测试/嵌入式构造（`src/api/legacy.rs:752` 说明它是可选注入）。此时不能报错，否则所有现有 `setup_complete` 集成测试（`tests/api_web/web_auth.rs:58` 等）都会失败 |
| 延迟任务里 `request_restart()` 返回 `false` | 保持 201 + `restarting: true`，`info!` 一条日志 | 一次性句柄（`src/application/restart.rs:20-26`），`false` 的语义是“已经在准备重启”，例如数据库切换流程（`src/api/users.rs:546`）刚排过；对客户端而言结果相同 |
| setup 已完成后再调 `setup_complete` | 409 `SETUP_ALREADY_COMPLETED`（`src/api/users.rs:753`），**不排期重启** | 该分支在 `setup.complete` 失败后就返回，早于排期代码；语义上也应如此（幂等读，不产生副作用） |
| 两个 `setup_complete` 并发 | 只有一个插入成功：`insert_initial_user` 是单事务里的 `INSERT ... SELECT ... WHERE NOT EXISTS (SELECT 1 FROM users)`（`src/storage/users.rs:717-734`），失败方拿到 `SetupAlreadyCompleted` → 409 | 排期只在成功方执行一次；失败方的 409 可能正好撞上重启窗口而被连接重置，客户端重试会得到同样的 409（可重入） |
| `exec` 本身失败（非 unix，或 `current_exe` 失败） | `restart_process` 返回 `Err` → `serve` 返回 `Err` → `main` 返回错误，进程**退出**而不是继续服务 | 这是既有行为（`src/main.rs:346-359`），数据库切换重启也一样；容器/守护进程重启策略负责拉起。setup 数据已提交且 `database.close()` 已执行（`src/main.rs:281`），不会丢 |

## 5. 设计问题 4：前端

### 5.1 现状

- `SetupPage` 已经有一套“正在重启”引导，但它服务于**数据库切换**：`restartRequested` 状态
  （`web/src/features/auth/SetupPage.tsx:11`）、轮询 `setupDatabaseStatus`（`:16`）、全屏重启提示
  （`:32-33`）。它之所以不会“提前判定就绪”，是因为 `restartRequired` 在重启真正发生前一直是
  `true`（数据库侧持久标记），重启后才翻转。
- setup 完成后的流程完全不同：`AdminSetupForm` 在 `onSuccess` 里直接
  `queryClient.setQueryData(queryKeys.setup, { initialized: true })` 然后 `navigate("/login")`
  （`web/src/features/auth/AdminSetupForm.tsx:24-27`）。**注意**：`initialized` 在 201 之后立刻就是
  `true`（管理员已落库），而进程还没重启，所以用“`initialized === true` 即就绪”的判据会提前放行。
- 客户端 `setup()` 目前只声明 `{ user: LuxUser }`（`web/src/lib/api/client.ts:426-436`），
  数据库重启已有 `{ restarting: boolean }` 的先例（`client.ts:420-423`）。

### 5.2 改动

| 文件 | 改动 |
| --- | --- |
| `web/src/lib/api/client.ts` | `setup()` 返回类型改为 `{ user: LuxUser; restarting?: boolean }`（`restarting` 可选：兼容旧后端） |
| `web/src/lib/api/types.ts` | 若需要，加 `SetupCompleteResult` 类型（可选字段 `restarting?: boolean`） |
| `web/src/features/auth/AdminSetupForm.tsx` | `onSuccess` 读 `result.restarting`：`true` → **不写** `queryKeys.setup`、不 `navigate`，改为调父组件传入的 `onRestarting()`（否则 `App.tsx:89` 的 `!setup.data.initialized` 判据会立刻把页面切走，重启提示根本没机会出现）；`false/undefined` → 保持现有行为（`setQueryData` + `navigate("/login")`） |
| `web/src/features/auth/SetupPage.tsx` | 持有 `phase: "form" \| "restarting"`；`restarting` 时渲染与数据库流程同款的全屏提示（复用 `:33` 的 `lux-state-screen` 结构，文案改为“初始化已完成，正在重启 Lux…”），轮询就绪后再 `setQueryData(queryKeys.setup, { initialized: true })` 让 `App` 切到登录路由 |
| `web/tests/setup-page.test.tsx` | 新增：`restarting: true` 的 201 后出现“正在重启 Lux”；就绪后进入登录；`restarting: false` 仍是直接跳转 |

### 5.3 轮询就绪的方式（关键细节）

因为 `setup/status` 在 201 之后**立刻**返回 `initialized: true`，不能用“一次成功即就绪”。用
“先看到断开、再看到恢复”的判据：

1. 进入 `restarting` 后每 1 s 调一次 `api.setupStatus()`（沿用 `SetupPage.tsx:16` 的
   `refetchInterval` 写法）。
2. 状态机 `waiting-for-down → waiting-for-up → ready`：
   - 第一次**网络层失败**（`fetch` 抛 `TypeError`，说明端口已关）→ 转 `waiting-for-up`；
   - 之后第一次成功 → `invalidateQueries(queryKeys.setup)` 并 `navigate("/login")`；
   - 兜底：10 s 内一次失败都没出现（重启被跳过、或反向代理把请求缓存/重试掉了）→ 直接进入登录页，
     登录请求本身会重试，失败时用户看到错误并可再提交。
3. 出现 `restarting === false`（旧后端或无重启句柄）→ 完全走今天的路径，不显示重启页。
4. 就绪后的收尾：`setQueryData(queryKeys.setup, { initialized: true })`（`App.tsx:89` 据此切到登录
   路由）+ `removeQueries(queryKeys.me)`（沿用 `AdminSetupForm.tsx:26`），然后 `navigate("/login")`。

反向代理部署提示：重启窗口内 `502/504` 由代理产生，同样算“连接失败”，与 `TypeError` 一起按失败处理。

### 5.4 兼容性

- **旧前端 + 新后端**：旧前端不认识 `restarting`，会立刻 `navigate("/login")`，登录请求可能撞上重启窗口
  失败一次，用户重试即可 → 降级可用，不阻塞发布。
- **新前端 + 旧后端**：`restarting` 缺省 → 当作 `false`，行为与今天一致。
- **Emby 客户端**：不走 setup（`docs/LUX-DEVELOPMENT.md:1620-1626` 列出的 setup 端点与 Emby 路由/DTO
  是两套；`setup_complete` 只有 Web 初始化页调用），因此没有协议兼容问题。它们只会经历与现有数据库
  切换重启相同的数秒连接中断，随后按既有重试逻辑恢复。这一点与方案 B 的所有者决策一致（已接受）。

## 6. 设计问题 5：测试策略

### 6.1 为什么测试里不会真的 `exec`

`exec` 只存在于 bin 目标的 `main.rs:346-359`，调用点 `main.rs:284`。集成测试用
`app_with_state(...)` 在进程内起 axum（`tests/api_web/web_auth.rs:24-33`），且默认**没有**重启句柄；
只有显式 `.with_restart_handle(..)` 才注入我们自己的 `watch` 通道。因此“响应先于重启”可以在进程内
完整断言，不需要替身框架。

### 6.2 后端用例（目标组见 `docs/TEST-TARGETS.md`）

| 用例 | 位置/命令 | 断言 |
| --- | --- | --- |
| `setup_complete_requests_a_restart_after_the_response` | `tests/api_web/web_auth.rs`，`cargo test --locked --test api_web web_auth::`（`docs/TEST-TARGETS.md:60`） | 注入 `with_restart_handle` + `with_setup_restart_delay(200ms)`，并在测试里持有 `watch::Receiver`：拿到 `201` + `restarting: true` 之后**立即**断言 `*rx.borrow() == Running`（响应已送达、重启尚未排上），再 `timeout(5s, rx.changed())` 断言变成 `Restart` |
| `setup_complete_without_a_restart_handle_keeps_the_created_body` | 同上 | 201、`restarting: false`、`user`/`library`/`scanJob` 字段与今天逐字一致（防止“为了重启而改动响应形状”） |
| `setup_complete_does_not_restart_again_when_the_admin_exists` | 同上 | 先完整跑一次成功初始化（重启已排上，等 `rx.changed()` 观察到 `Restart`），再用同一 state 发第二次 POST：得 409 `SETUP_ALREADY_COMPLETED`，且后续 `timeout(300ms, rx.changed())` 超时（`watch` 不会再有第二次变更） |
| `setup_complete_restarts_even_when_the_first_library_is_rejected` | 同上 | 非法 `rootPath` → 4xx，但随后仍观察到 `Restart`（对应 §4.2 的部分成功策略） |
| `startup_resumes_first_scans_interrupted_by_shutdown` | `tests/scanner/shutdown_jobs.rs`，`cargo test --locked --test scanner shutdown_jobs::`（`docs/TEST-TARGETS.md:91`） | 建库 + 加根 + 建扫描任务 → `cancel_incomplete_jobs_for_shutdown()` → 跑恢复流程 → 该库出现新的 `PENDING/RUNNING` 任务，`libraries.last_scan_at` 仍为 `NULL` |
| 恢复流程的反向用例 | 同上 | ① `last_scan_at` 已写 → 不恢复；② 任务 `error != 'SERVER_SHUTDOWN'`（真实失败）→ 不恢复；③ 库已停用 → 不恢复；④ 库没有 root → 不恢复（这一条正是 SQL 里 `EXISTS(library_roots)` 的作用，`start_library_scan` 本身不拒绝零 root 的库，见 `src/application/scanner/jobs/lifecycle.rs:910`）；⑤ 连跑两次 → 只产生一个活动任务（第二次走 `AlreadyActive` 跳过） |
| `restart_handle` 一次性 | `src/application/restart.rs:33-45`（已有） | 复用现有单元测试，不新增 |

**为什么 200 ms 的时序断言不会 flaky**：201 是本地 reqwest 往返（毫秒级以内），200 ms 是三个数量级
以上的余量；上界用 5 s 超时。若阶段 2 实测发现 CI 抖动，可把注入延迟提到 500 ms，或改用
`tokio::time::pause()/advance()`（但暂停时钟下 `tokio` 会在空闲时自动推进，断言顺序反而不可控，
故不默认使用）。

**默认延迟不拖慢现有测试**：只有注入了重启句柄的用例才会 spawn 延迟任务；现有所有
`setup/complete` 用例（`tests/api_web/web_auth.rs:58`、`tests/library/libraries_api.rs:113`、
`tests/storage/database_setup_api.rs:78` 等）都没有句柄，走 `restarting: false` 分支，零等待。

### 6.3 前端用例

`web/tests/setup-page.test.tsx`（现有 mock 风格见 `:69-90`）：

1. `setup` mock 返回 `{ user, restarting: true }` → 完成初始化后出现“正在重启 Lux”与 `aria-busy`，
   且**没有**立即跳转登录；
2. 让 `setupStatus` 的第一次调用 reject（模拟端口关闭）、第二次 resolve `{ initialized: true }` →
   最终进入登录页；
3. `restarting` 缺省（旧后端）→ 行为与今天一致（直接跳转）。

命令：`pnpm --dir web exec vitest run tests/setup-page.test.tsx`，另跑 `pnpm --dir web test`、
`pnpm --dir web build`（`AGENTS.md` 的 Web 门禁）。

### 6.4 不做的自动化

“真正 `exec` 后服务重新可用”的端到端验证不进 CI：`tests/shutdown.rs:59` 已经有“spawn 真实二进制 +
发信号”的夹具，但重启会把测试进程自身 `exec` 掉，需要额外的子进程隔离与端口/配置注入。阶段 2 在
本机手动验证一次（Docker 与裸进程各一次），并在 `docs/LUX-DEVELOPMENT.md` 的任务条目里记录。

## 7. 设计问题 6：回滚

### 7.1 提交切分（每步可单独回退）

| 提交 | 内容 | 单独回退后的状态 |
| --- | --- | --- |
| C1 | `setup_complete` 排期重启 + `restarting` 字段 + `CoreState.setup_restart_delay`（含注入辅助） | setup 不再重启，其余不变；响应里少一个可选字段（旧前端本来就不读）。恢复流程 C2 失去生产者，但只会对“任何被重启打断的首次扫描”生效，无害 |
| C2 | `scan_jobs` 恢复查询 + `resume_shutdown_interrupted_first_scans` + `main.rs` 接线 | setup 创建的库在重启后不会被自动扫描（回到今天的行为）；重启行为不受影响 |
| C3 | 前端重启引导 + `setup()` 返回类型 | 后端照样重启；旧界面在 201 后立刻跳登录，可能失败一次让用户重试 → 降级可用 |
| C4 | `docs/SETUP-RESTART-DESIGN.md`、`docs/LUX-DEVELOPMENT.md` 任务条目 | 纯文档 |

### 7.2 与方案 A 的隔离

- 三个代码提交都**不触碰** `ReadyServices`（`src/api/legacy.rs:259` 起的 `AppState`）、
  `require_ready!`（`src/api/mod.rs:51`）与路由注册（`src/api/users.rs:3122` 的 `api_routes`），
  因此方案 A 的步骤 1+2（已合并）不受回滚影响。
- **无 schema 变更**：推荐方案 (b) 不新增表/列（这正是它相对 (a1)/(c) 的额外优势），回退代码后数据库
  不需要修复或迁移；`scan_jobs` 里的取消记录本来就属于“被关停打断”的既有语义。
- **回滚边界是第 ③ 步之前**：如果第 ③ 步（Router 拆分）已经合并，单独回退 C1 会让进程永远停在
  setup 模式（ready 路由没挂载），此时必须同时回退第 ③ 步。因此第 ③ 步应在本文档验证通过、稳定一个
  发布周期后再动。

### 7.3 线上救火（不回退代码）

- 最轻的开关是“延迟注入”的公开形态：阶段 2 可决定是否把它接到 `Config`（例如
  `LUX_SETUP_RESTART_DELAY_MS`，默认 250）。**建议先不加环境变量**（项目惯例是不为假设需求加配置），
  真的出现“重启过早”问题时，把 `setup_restart_delay` 的默认值调大即可，仍只是一个字段默认值改动。
- 若首次扫描恢复出现异常（重复扫描/扫描风暴），单独回退 C2 即可止血，不影响 setup 流程本身。

## 8. 时序与数据流

### 8.1 请求与重启（文字时序）

```
Web                setup_complete                main::serve             重新 exec 的进程
 |   POST /setup/complete |                            |                        |
 |----------------------->|  1 建管理员（事务，WHERE NOT EXISTS）              |
 |                        |  2 可选建库 + 加根 + start_library_scan            |
 |                        |     （先写 scan_jobs 行 = PENDING，再 spawn worker）|
 |                        |  3 记 admin_created = true                         |
 |                        |  4 响应体加 restarting: true                        |
 |                        |  5 spawn: sleep(250ms) → request_restart()          |
 |<--- 201 + body --------|      （响应同步返回，不等待第 5 步）                |
 |                        |                            | watch 变 Restart       |
 |                        |                            | 停止接入、排空在途请求 |
 |                        |                            | （本响应此时早已写出） |
 |                        |                            | 停后台 worker          |
 |                        |                            | 取消未完成任务         |
 |                        |                            |  → 首次扫描任务变      |
 |                        |                            |    CANCELLED/SHUTDOWN  |
 |                        |                            | database.close()       |
 |                        |                            | exec(自己) ----------->|
 |  轮询 setup/status ----|---- 连接失败（端口关闭）    |                        |
 |  （前端进入“正在重启”） |                            |                        | 启动：
 |                        |                            |                        |  迁移 + 启动取消
 |                        |                            |                        |  build_app_state：
 |                        |                            |                        |   resume 首次扫描
 |                        |                            |                        |   （谓词命中 → 新任务）
 |<-- 200 initialized:true|                            |                        |
 |  跳转 /login，管理员登录 |                            |                        |
```

要点：

1. **数据先于重启**：管理员、库、根、扫描任务都在 201 之前落库（`src/api/users.rs:692-751`）。
2. **重启先于响应送达是安全的**：`with_graceful_shutdown`（`src/main.rs:257`）会排空在途请求，响应
   不会因为 watch 提前变化而丢失；250 ms 只是避免“watch 变更”与“写首字节”竞争。
3. **取消是最后一步动作**：`cancel_incomplete_jobs_for_shutdown` 在 `serve` 返回之后才执行
   （`src/main.rs:271`），所以它取消的一定是“已经返回过 201 的那次扫描”。
4. **恢复在新进程的 `build_app_state` 里**：与 `resume_interrupted_metadata_scraping`
   （`src/main.rs:202`）同一位置、同一后台语义。

### 8.2 恢复流程的数据流

```
scan_jobs(CANCELLED, error='SERVER_SHUTDOWN', job_type='RECONCILE_LIBRARY')
   ⋈ libraries(last_scan_at IS NULL, is_enabled=1)
   ⋈ EXISTS library_roots
   ⋈ NOT EXISTS scan_jobs(status IN ('PENDING','RUNNING'))     ← 去重 / 避免并发
        ↓  每库取一条（GROUP BY library_id，单次启动最多 32 个）
start_library_scan(scan_jobs, library_id, workers)             ← 复用 setup 用的同一个入口
        ↓
新 scan_jobs 行(PENDING) + 新 scan_manifest + spawn worker
        ↓  扫描成功
libraries.last_scan_at = unixepoch()                           ← 下次重启不再命中谓词
```

## 9. 涉及文件清单

### 9.1 阶段 2（实现）——后端

| 文件 | 改动 |
| --- | --- |
| `src/api/users.rs` | `setup_complete`：`admin_created` 标记、`restarting` 字段、handler 末尾统一排期；新增 `setup_restart_response` 帮助函数 |
| `src/api/legacy.rs` | `CoreState` 增 `setup_restart_delay: Duration`（默认 250 ms，`src/api/legacy.rs:175/180`），新增 `#[doc(hidden)] with_setup_restart_delay`；新增 `AppState::resume_shutdown_interrupted_first_scans()`（放在 `resume_interrupted_metadata_scraping` 旁，`:953`） |
| `src/main.rs` | `build_app_state` 里 `src/main.rs:202` 之后加一行调用 |
| `src/application/admin_scan_jobs.rs` | 新增 `resume_shutdown_interrupted_first_scans(scan_jobs, workers, limit)`，内部复用 `start_library_scan`（`:143`） |
| `src/storage/jobs/scan/lifecycle.rs` | 新增 `list_shutdown_interrupted_first_scan_library_ids(after, limit)`（与 `find_active_scan_job_for_library`（`:235`）同模块） |
| `tests/api_web/web_auth.rs` | §6.2 的 4 个 HTTP 用例 |
| `tests/scanner/shutdown_jobs.rs` | §6.2 的恢复流程用例与反向用例 |
| `docs/LUX-DEVELOPMENT.md` | 追加本任务的 `LUX-*` 条目与验收结果（阶段 2 末尾） |
| `docs/API.md` | `POST /api/v1/setup/complete` 的响应说明补 `restarting`（`:20`） |

### 9.2 阶段 2（实现）——前端

| 文件 | 改动 |
| --- | --- |
| `web/src/lib/api/client.ts` | `setup()` 返回类型加可选 `restarting`（`:426-436`） |
| `web/src/lib/api/types.ts` | 可选：`SetupCompleteResult` 类型 |
| `web/src/features/auth/AdminSetupForm.tsx` | `onSuccess` 分支（`:24-27`）：`restarting` 时交给父组件的重启引导 |
| `web/src/features/auth/SetupPage.tsx` | 重启阶段状态机 + 轮询就绪（复用 `:32-33` 的全屏样式） |
| `web/tests/setup-page.test.tsx` | §6.3 的 3 个用例 |

## 10. 风险表

| # | 风险 | 影响 | 缓解 | 等级 |
| --- | --- | --- | --- | --- |
| R1 | 201 在重启时丢失（客户端看不到成功结果） | 用户以为初始化失败，重复提交得到 409 | graceful shutdown 排空在途请求（`src/main.rs:257`）+ 250 ms 延迟 + §6.2 的顺序断言 | 低 |
| R2 | 首次扫描被取消后没有恢复 | 首个库长期为空，且没有任何提示 | §3.3 的启动恢复；`scanJob` 仍出现在 201 里便于排查 | 低 |
| R3 | setup 时扫描**没能启动**（无任务行 ⇒ 无恢复证据） | 同上，但只能手动扫描 | 201 里 `scanJob` 为 `null`；`warn!` 日志（`src/api/users.rs:732`）；`docs/LUX-DEVELOPMENT.md` 的部署说明提示“若首库为空请手动扫描”。若要彻底覆盖需改用 (a1) 标记 | 中 |
| R4 | 恢复谓词过宽导致批量重扫 | 升级后启动即全量扫描，IO 抖动 | 谓词要求“存在被关停打断的 `RECONCILE_LIBRARY` 任务”+`last_scan_at IS NULL`，历史库没有该证据（§3.1 证据 4）；单次启动上限 32 | 低 |
| R5 | 前端把“还没重启”误判为“已就绪” | 登录请求撞上关停窗口，报一次网络错误 | §5.3 的 “先断开再恢复” 状态机 + 10 s 兜底 | 低 |
| R6 | 重启窗口内 Emby/其它客户端断连数秒 | 播放/刮削请求失败重试 | 所有者已接受；与数据库切换重启同构（`src/api/users.rs:546`）；恢复后连接自然重连 | 已接受 |
| R7 | `exec` 失败导致进程退出 | 服务不可用直到守护进程拉起 | 既有行为（`src/main.rs:346-359`）；文档记录对容器 `restart: unless-stopped` 的依赖 | 低 |
| R8 | 部分成功（管理员已建、库建失败）后仍重启，用户只看到错误 | 用户困惑“为什么报错还重启” | 错误提示保持原样；重启后页面进入登录，管理员可自助建库。§4.2 已说明这是刻意的 | 低 |
| R9 | 阶段 3 之后单独回退本步 | setup 模式无法退出 | §7.2 的回滚边界：第 ③ 步必须在本步稳定后再动 | 中 |

## 11. 被否决的方案与理由

| 否决项 | 理由 |
| --- | --- |
| **(a1)** setup 不再启动扫描，改用 `lux_meta` 标记驱动重启后扫描 | 需要新 storage 公共 API（`lux_meta` 目前只在 `src/storage/database_cleanup.rs:384-401` 内部使用），引入“扫描意图”的第二处真源；201 的 `scanJob` 变 `null` 需要同步改 `docs/API.md` 与前端类型。仅作为 R3 的备选 |
| **(a2)** 只用 `last_scan_at IS NULL` 作启动谓词 | `last_scan_at` 只有 manifest 时代的 3 个写入点、无历史回填（§1），升级后首次启动会给所有历史库排队全量扫描 |
| **(c)** 关停时豁免 setup 创建的扫描任务 | 破坏 `src/storage/jobs/shutdown.rs:9-12` 的不变量，需要迁移；续跑还要处理半成品 manifest（`:18-38`） |
| **(d)** 前端就绪后调 `POST /admin/libraries/{id}/scan` | 不构成保证（关页面、代理抖动、管理员未登录都会漏），不能作为唯一机制 |
| **(e)/(f)** 在请求里等扫描完成 / 把重启延迟拉长到扫描完成 | 违反 `AGENTS.md`“不要在用户请求里扫描整个库”，201 会悬挂数分钟 |
| 固定 5–10 s 延迟来“保证”响应已送达 | 延迟换不来保证，只是把失败概率换了个位置；真正的保证来自 graceful shutdown 排空，延迟保持 250 ms |
| 响应 body 完成信号（`http_body` 包装器）作为**必须项** | 复杂度（约 40 行转发样板）与收益不成比例；保留为可选加固（§2） |
| `restart_handle` 为 `None` 时返回 503/500 | setup 已成功，不能因为“没有重启能力”报错；且会让所有现有集成测试失败 |
| 只在 201 分支排期重启 | 部分成功会停在 setup 模式（第 ③ 步之后等于死锁），见 §4.2 |
| 前端用 `setup/status.initialized` 一次成功判就绪 | `initialized` 在 201 之后立刻为 `true`（`src/api/users.rs:229-241` 只查 `has_users()`），会提前放行 |

## 12. 阶段 2 实施顺序（TDD，小步提交）

1. **C1 重启排期**（后端）：先写 `setup_complete_requests_a_restart_after_the_response` 与
   “无句柄仍返回 201”两个用例（预期失败）→ 加 `CoreState.setup_restart_delay` + 注入辅助 + 排期逻辑
   + `restarting` 字段 → `cargo test --locked --test api_web web_auth::`。
2. **C1b 幂等用例**：重复 `setup/complete`（409，不排期）与“库建失败仍重启”两个用例 → 同上命令。
3. **C2 首次扫描恢复**：先在 `tests/scanner/shutdown_jobs.rs` 写恢复与 5 个反向用例（失败）→ 加 storage
   查询、application 函数、`AppState` 方法、`main.rs` 接线 → `cargo test --locked --test scanner shutdown_jobs::`。
4. **C3 前端**：先在 `web/tests/setup-page.test.tsx` 写 3 个用例（失败）→ 改 `client.ts`/`types.ts`/
   `AdminSetupForm.tsx`/`SetupPage.tsx` → `pnpm --dir web exec vitest run tests/setup-page.test.tsx`
   → `pnpm --dir web test` → `pnpm --dir web build`。
5. **C4 文档**：`docs/API.md` 补 `restarting`；`docs/LUX-DEVELOPMENT.md` 追加任务条目与验收结果；
   本文件状态改为“已实施”。
6. **门禁**：`cargo build --locked`、`cargo test --locked --all-targets`、`cargo fmt --all -- --check`、
   `cargo clippy --locked --all-targets --all-features -- -D warnings`、
   `python3 scripts/check-migration-parity.py`、`python3 scripts/check-backend-branches.py`；
   手动在 Docker 与裸进程各验证一次真实重启（记录 `uname -m`）。

## 13. 需要确认的三个决策点

1. **首次扫描的恢复策略**：采用 (b)（复用 `scan_jobs` 的 `CANCELLED/SERVER_SHUTDOWN` 作为恢复证据，
   无 schema 变更），还是 (a1)（`lux_meta` 显式标记，能覆盖“扫描根本没启动”的遗漏面 R3）？
   本设计推荐 (b)，并把 (a1) 记为 R3 的备选。
2. **部分成功也重启**：管理员创建成功、但首个库因 4xx 失败时仍排期重启（§4.2）。这是为了第 ③ 步之后
   不把进程卡在 setup 模式；代价是用户看到错误后仍会经历一次重启。
3. **不加环境变量开关**：延迟只做可注入字段，不做 `LUX_SETUP_RESTART_*` 配置（§7.3）。若希望线上有
   紧急关闭手段，需要额外一个 `Config` 字段，本设计默认不做。
