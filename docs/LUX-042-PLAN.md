# LUX-042：实时监听、防抖和事件合并实施计划

## 范围

监听已配置媒体根目录，将新增、修改、重命名和删除事件转成规范化路径事件，使用有界通道和短 debounce 窗口合并同一路径的连续变化。

## 规则

- 使用 notify 推荐 watcher；watcher 回调不执行扫描或数据库 I/O。
- 事件通道容量固定为 256，满时丢弃并累计 dropped counter，交给后续全量调和兜底。
- 事件路径必须位于被监听根目录内；删除事件保留原路径，不要求目标仍存在。
- 同一路径在 debounce 窗口内只输出一条；Create+Modify 合并为 Create，Remove+Create 合并为 Modify。
- Rename 两端路径都保留为 Rename 事件，后续增量扫描用指纹和路径发现处理。

## 增量任务

- [x] 新增 notify watcher 和有界事件通道。
- [x] 实现路径规范化、事件分类和 debounce/coalescing。
- [x] 增加新增、修改、重命名、删除和洪峰有界测试。

## 验证门

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- ARM64 本机用临时目录验证真实文件事件和合并结果。

验证结果：以上检查均通过；本机 `arm64` / `aarch64-apple-darwin` 已验证真实创建/修改/重命名/删除事件、同路径合并和 256 容量边界。

## 明确不做

- 不让 watcher 直接执行扫描或阻塞前台请求。
- 不把实时事件作为 missing/delete 的唯一事实来源（LUX-043）。
