# LUX-062 Emby Seasons/Episodes/NextUp

## 范围

把 LUX-060 的 SERIES/SEASON/EPISODE 层级通过 Emby 兼容路由暴露，并从 `user_item_state` 读取每个用户的播放位置、已播放、收藏和播放次数。

## 实现

- [x] 新增 `GET /Shows/{seriesId}/Seasons`，按季度号稳定排序并执行父系列 ACL。
- [x] 新增 `GET /Shows/{seriesId}/Episodes`，支持 `SeasonId`、`StartIndex`、`Limit`，按季号/集号排序。
- [x] 新增 `GET /Users/{userId}/Items/NextUp`，只返回该用户有播放位置但未标记已播放的单集。
- [x] 新增 `user_item_state` 持久表；Episode DTO 的 `UserData` 读取位置、播放次数、收藏和已播放状态。
- [x] Series/Season/Episode DTO 返回 `ParentId`、`SeriesId`、`ParentIndexNumber`、`Index` 和正确的 `IsFolder`。

## 验证

- 协议集成测试覆盖季度、全剧集、指定季度、分页、排序和 NextUp。
- 测试覆盖用户状态映射及无媒体库权限用户的 404/空结果边界。
- 既有电影目录、详情、图片 ACL 和迁移检查保持通过。

## 明确不做

- 本阶段只读取用户状态，不实现播放进度/收藏写入；写入端点属于后续播放与进度阶段。
- 本阶段不增加剧集 Web UI、字幕端点或混合库分类。
