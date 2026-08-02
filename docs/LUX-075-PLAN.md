# LUX-075 收藏与已看 API

## 范围

Emby 和 Lux API 共同操作 `user_item_state` 的已看与收藏字段，重复请求保持幂等，并以条目 ACL 防止越权探测。

## 实现

- [x] Emby `POST/DELETE /Users/{userId}/PlayedItems/{itemId}`。
- [x] Emby `POST/DELETE /Users/{userId}/FavoriteItems/{itemId}`。
- [x] Lux `PUT /api/v1/items/{itemId}/favorite`。
- [x] 重复 POST/DELETE 不重复增加 PlayCount；不同用户状态隔离。
- [x] 无媒体库权限的条目按 404 返回。

## 验证

- 集成测试覆盖 Resume、管理员阈值、已看、收藏、重复操作和无权限用户。

## 明确不做

- 本阶段不实现批量收藏、合集收藏和跨用户管理操作。
