# LUX-193 演员收藏

## 范围

为 Lux Web 的演员/人物详情增加按用户隔离的收藏状态。演员收藏与媒体条目的
`user_item_state` 分开存储，不改变 Emby 人物 DTO 和 Emby 兼容收藏接口。

## 接口契约

- `GET /api/v1/people/{personId}` 在人物 DTO 中返回 `isFavorite`。
- `PUT /api/v1/people/{personId}/favorite` 接收 `{ "favorite": true|false }`，成功返回
  `204 No Content`。
- 修改接口需要登录和现有 CSRF 校验；人物不在当前用户可访问媒体库中时返回 `404`，避免
  越权探测。
- 同一用户重复设置收藏或取消收藏保持幂等；不同用户状态相互隔离。

## 验收

- [ ] 从空 SQLite 和 PostgreSQL 数据库执行迁移成功。
- [ ] 人物详情能读出当前用户的收藏状态。
- [ ] 收藏、取消收藏、重复请求和不同用户隔离有 Rust 集成测试。
- [ ] Web 人物详情提供可访问的收藏切换按钮，并在成功后刷新人物状态。
- [ ] Web API 客户端和人物详情组件有自动化测试。
- [ ] Rust/Web 基线检查通过。

## 明确不做

- 不把演员收藏混入 Emby `FavoriteItems` 或媒体条目的 `user_item_state`。
- 不在本任务增加演员收藏列表页面；后续如需要，单独设计分页列表接口和页面。
