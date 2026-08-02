# LUX-040：文件指纹与扫描 generation 实施计划

## 范围

完善 LUX-031 扫描器的快速 fingerprint 和完整扫描 generation：同一路径未变化时只做 stat/指纹比较，仍更新本轮 seen；扫描成功后将本轮未见 entry 标记为 missing。

## 规则

- fingerprint 版本化，包含规范化相对路径、文件大小、高精度修改时间，以及 Unix 可用的 device/inode。
- 不对媒体内容做全文件哈希；指纹只用于发现变化。
- generation 使用 UUIDv7 字符串，在一次库扫描的所有根路径中保持一致。
- 未变化 entry 更新 `last_seen_generation` 并清除 `is_missing`，不重建 item/source。
- 只有根路径成功完成 readdir/stat 后才执行 missing 标记。

## 增量任务

- [x] 实现稳定 fingerprint 计算和高精度修改时间。
- [x] 扫描器持久化 fingerprint，并在未变化路径更新本轮 generation。
- [x] 完整扫描后按 generation 标记未见 entry missing。
- [x] 增加稳定指纹、重复扫描和 missing fixture 测试。

## 验证门

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- ARM64 本机验证同一树第二次扫描不创建新 source，且探测任务为零。

验证结果：以上检查均通过；本机 `arm64` / `aarch64-apple-darwin` 已验证 fingerprint 稳定性、重复扫描、generation seen、missing 标记，以及同一 NFO 的二次解析跳过。

## 明确不做

- 不实现持久化扫描任务、游标恢复、实时监听或事件防抖（LUX-041/042）。
- 不对所有大文件进行全文件内容哈希。
