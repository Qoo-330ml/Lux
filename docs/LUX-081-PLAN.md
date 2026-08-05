# LUX-081 媒体库筛选和排序

## 范围

在 Emby Items 和 Lux 媒体库列表上统一映射类型、年份、已看、收藏、名称/加入日期/发行日期/评分排序和分页参数。

## 实现

- [x] Emby Items 支持 `IncludeItemTypes`、`IsPlayed`、`IsFavorite`、`Years`、`SortBy`、`SortOrder`。
- [x] Lux 媒体库列表支持 `itemType`、`year`、`isPlayed`、`isFavorite`、`sortBy`、`sortOrder`；`sortBy` 支持名称、加入日期、发行日期和评分。
- [x] 筛选在 ACL 之后执行，分页在筛选排序之后执行。
- [x] 默认名称排序和稳定 ID tie-breaker 保持确定性。

## 验证

- 复用已看/收藏集成 fixture 验证组合筛选和分页。
- 既有目录、剧集、搜索和 ACL 测试保持通过。

## 明确不做

- 本阶段不实现 Genres/Tags/BoxSet 等高级筛选，后续按客户端真实请求增加。
