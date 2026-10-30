# setup 完成后重启进程（方案 B 第 ② 步设计）

日期：2026-11-04。状态：**设计待确认（尚未改任何 `src/**`、`web/**`）**。范围：`setup_complete`
成功路径的重启触发、首次扫描的存续、前端“正在重启”引导，以及对应的测试与回滚策略。

本文件是 `docs/APP-STATE-NEXT.md` 方案 B 的**第 ② 步**：先让“setup 完成后重启进程”成为既有行为，
第 ③ 步（拆 `SetupState`/`ReadyAppState` 与 `setup_router()`/`ready_router()`）才成立。本文档不含
Router 拆分。

## 0. 一句话结论

- 重启请求在 201 响应**构造完成后**由后台任务延迟约 250 ms 发出；响应的送达由 `axum::serve` 的
  graceful shutdown 保证（而非靠延迟“猜”）。
- setup 里**继续**启动首次扫描（不改 201 响应中的 `library`/`scanJob`），改为在启动流程中恢复
  “被关停打断的首次扫描”。理由是这份“扫描意图”已经作为 `scan_jobs` 行落库，不需要第二处真源。
- setup 已成功的请求**永不因重启机制失败**：`restart_handle` 缺失时返回 201 + `restarting: false`。

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

**做法**（handler 末尾，成功分支）：

1. 构造完响应 JSON（含 `restarting` 字段，见 §4），克隆 `state.restart_handle`。
2. `tokio::spawn` 一个延迟任务：`sleep(delay)` → `handle.request_restart()`；失败只记 `info` 日志
   （一次性句柄，说明已有其它调用排在前面）。
3. **同步返回** 201，不等待该任务。

```rust
// src/api/users.rs，成功分支末尾（示意）
if let Some(handle) = state.restart_handle.clone() {   // core 状态字段，见 src/api/legacy.rs:180
    let delay = state.setup_restart_delay();           // 新增：默认 250 ms，测试可注入
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        if !handle.request_restart() {
            tracing::info!("restart was already requested before setup completed");
        }
    });
    response["restarting"] = json!(true);
} else {
    response["restarting"] = json!(false);             // 嵌入式/测试状态，见 §4
    tracing::warn!("setup completed without a restart handle");
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

（待填）

## 4. 设计问题 3：幂等与失败

（待填）

## 5. 设计问题 4：前端

（待填）

## 6. 设计问题 5：测试策略

（待填）

## 7. 设计问题 6：回滚

（待填）

## 8. 时序与数据流

（待填）

## 9. 涉及文件清单

（待填）

## 10. 风险表

（待填）

## 11. 被否决的方案与理由

（待填）

## 12. 阶段 2 实施顺序（TDD，小步提交）

（待填）
