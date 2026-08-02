# LUX-034：电影查询与 Emby Items 实施计划

## 范围

在已有电影索引和媒体探测结果上提供默认分页的 Lux 查询接口，并补齐客户端需要的 Emby 用户媒体库、Items 列表和详情 DTO。当前阶段所有已认证用户读取同一已启用媒体库；更细的媒体库 ACL 留给后续权限任务。

## 规则

- 所有 Lux 媒体查询要求有效 Web session；Emby 查询要求有效 access token。
- Emby 用户路径中的 user id 必须与 token 用户一致，管理员可查看其他用户路径但不绕过令牌校验。
- `startIndex`/`limit` 使用 Emby 分页字段，默认 0/50，limit 上限 100。
- Lux API 使用 `page`/`pageSize`，默认 1/50，pageSize 上限 100。
- DTO 不暴露服务器内部路径；媒体源只返回客户端所需的容器、大小、时长、码率和轨道信息。
- 缺失 NFO/探测字段保持为空，不用空字符串覆盖已有值。

## 增量任务

### Slice 1：目录查询和 Lux API

- [x] 增加按库分页、按 id 详情的目录查询服务。
- [x] 实现 `/api/v1/libraries`、`/api/v1/libraries/{id}/items`、`/api/v1/items/{id}`。
- [x] 为标题、原名、年份、简介、时长、媒体源和轨道增加 DTO 测试。

### Slice 2：Emby Items 兼容

- [x] 增加 Emby token 到用户的解析。
- [x] 实现 `/Users/{userId}/Views`、`/Items`、`/Users/{userId}/Items`、详情和 Resume 空结果。
- [x] 增加 Items 列表/详情 golden shape 和未授权/越权测试。

## 验证门

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- ARM64 本机验证登录后 Lux/Emby 均能列出并查看电影，分页默认值生效。

验证结果：以上检查均通过；本机 `arm64` / `aarch64-apple-darwin` 已验证 Lux 与 Emby 的列表、详情、分页、API key 认证和用户路径隔离。

## 明确不做

- 不实现图片内容端点（LUX-035）。
- 不实现播放、进度、收藏和复杂搜索。
- 不实现后台任务队列或媒体库 ACL 管理。
