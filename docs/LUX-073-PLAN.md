# LUX-073 播放会话事件

## 范围

持久化 Emby 播放会话和进度事件，按用户、设备和 PlaySessionId 做幂等 upsert；进度只允许单调前进，停止会话从活动 Sessions 列表消失但保留最终进度。

## 实现

- [x] 新增 `POST /Sessions/Playing`、`/Sessions/Playing/Progress`、`/Sessions/Playing/Stopped`。
- [x] 新增 `GET /Sessions`，普通用户只能看到自己的活动会话，管理员可看到全部活动会话。
- [x] 会话记录保存用户、条目、媒体源、PlaySessionId、设备、客户端、暂停状态和位置。
- [x] 会话记录保存真实对端 IP；Emby `GET /Sessions` 暴露 `RemoteEndPoint`，管理仪表盘暴露可空的 `remoteIp`。
- [x] 会话 upsert 与 `user_item_state` 进度写入处于同一事务，重复事件不重复递增版本，乱序位置不回退。
- [x] 新增 Lux `GET /api/v1/items/{itemId}/playback` 和 `POST /api/v1/items/{itemId}/progress`。

## 验证

- 集成测试覆盖 Playing/Progress/Stopped、重复事件、并发乱序位置、设备会话查询和最终进度。
- 集成测试覆盖 Emby 会话和管理仪表盘返回播放来源 IP。
- ARM64 全量回归保持通过。

## 明确不做

- 已看阈值、继续观看清理和收藏操作属于 LUX-074/LUX-075。
- 本阶段不实现会话 WebSocket 或实时消息。
