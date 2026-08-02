# LUX-043：全量调和与根路径故障保护实施计划

## 范围

让全量扫描在根路径临时卸载、权限变化或恢复时安全运行：不可用根不执行 missing 标记，数据库记录 availability；恢复后重新检查并继续 generation。

## 规则

- 根路径 `readdir/stat` 失败时只标记该根不可用，不把已有文件批量标记 missing。
- 已标记不可用的根每次扫描先重新检查目录；恢复后清除 unavailable 状态。
- 只有本轮成功完整遍历的根才调用 `mark_missing_filesystem_entries`。
- 实时事件不参与删除判断，完整 generation 仍是 missing 的唯一来源。

## 增量任务

- [x] 增加根路径 availability 更新和恢复检查。
- [x] 扫描错误隔离到单个根，保护其他可用根继续扫描。
- [x] 增加卸载、恢复和真实删除的 generation 测试。

## 验证门

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- ARM64 本机验证根路径消失不触发 missing，恢复后可继续扫描。

验证结果：以上检查均通过；本机 `arm64` / `aarch64-apple-darwin` 已验证根目录消失时不标记 missing，恢复后重新可用，其他根仍可继续扫描。

## 明确不做

- 不实现 missing 宽限期和 purge 任务。
- 不实现 PollWatcher 回退；实时监听仍由 LUX-042 提供，定时 reconcile 负责兜底。
