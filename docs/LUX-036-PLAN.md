# LUX-036：基础媒体库 ACL 实施计划

## 范围

为普通用户增加媒体库级 `can_view` 授权，并在所有电影列表、详情和图片端点进入 application service 时复用同一个授权器。管理员默认拥有全部媒体库访问权。

## 规则

- 普通用户默认拒绝，只有 `user_library_access.can_view = 1` 才能查看。
- 管理员不需要为每个库写授权行。
- 列表无权库返回 403；已知 item/image ID 无权时按 404 处理，避免 ID 探测。
- Emby 用户路径权限和媒体库 ACL 同时检查；不能通过 `/Items/{id}` 或图片端点绕过。
- 管理员通过受保护的管理 API 授予或撤销访问。

## 增量任务

### Slice 1：授权模型与管理 API

- [x] 新增 `user_library_access` migration 和 storage upsert/query。
- [x] 实现统一 application authorizer，接入库、条目和图片判断。
- [x] 增加管理员授予/拒绝用户媒体库访问 API。

### Slice 2：查询端点接入

- [x] Lux/Emby 库列表和电影列表按 ACL 过滤。
- [x] Lux/Emby 详情和图片端点拒绝已知 ID 越权。
- [x] 增加两个用户、两个媒体库的权限矩阵集成测试。

## 验证门

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- ARM64 本机验证授予/撤销后列表、详情和图片权限一致。

验证结果：以上检查均通过；本机 `arm64` / `aarch64-apple-darwin` 已验证两个用户、两个媒体库的授予/拒绝矩阵，覆盖 Lux/Emby 列表、详情和图片。

## 明确不做

- 不实现内容分级、标签 ACL 或复杂角色系统。
- 不实现播放/下载权限；它们在后续任务继续复用 authorizer。
