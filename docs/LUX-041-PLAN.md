# LUX-041：持久扫描任务与游标实施计划

## 范围

为电影库扫描增加持久化 job 状态、批次进度、当前游标和取消标记。任务重启后依据数据库中的 `RUNNING` 状态和 cursor 继续，不重新处理已经提交的文件。

## 规则

- 每个媒体库同时只允许一个活动全量扫描任务。
- 每批默认 100 个媒体文件，批次完成后更新 job processed/cursor 和根路径 scan_cursor。
- 处理成功后才推进 cursor；进程在批次中途退出时从上一个已提交 cursor 重复一小批，单文件处理仍依靠 fingerprint 幂等。
- cancel 请求只设置持久化标志，worker 在批次边界停止并将任务标记 CANCELLED。
- 完成后标记 job COMPLETED、清理根路径 cursor 并更新 library last_scan_at。

## 增量任务

- [x] 新增 scan_jobs migration 和 job storage 状态机。
- [x] 抽取单文件扫描处理，按批次更新 job progress/cursor。
- [x] 实现重启恢复和取消 API/服务。
- [x] 覆盖中途恢复、取消、重复活动任务和批次幂等测试。

## 验证门

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- ARM64 本机验证中断后恢复、cursor 持久化和取消。

验证结果：以上检查均通过；本机 `arm64` / `aarch64-apple-darwin` 已验证分批 progress/cursor、重启恢复、活动任务去重和取消。

## 明确不做

- 不实现实时文件监听、防抖和事件合并（LUX-042）。
- 不实现跨进程 worker leader election；SQLite 活动 job 唯一约束负责首版去重。
