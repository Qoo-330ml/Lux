# Lux SQL 查询审计

本文记录当前 SQLite schema 的代表性热查询计划。它是 LUX-130 的可重复审计证据之一，不把少量代表性查询误写成“所有查询都已优化”。

## ARM64 检查

- 日期：2026-08-03
- 架构：macOS `arm64`
- 数据库：从空目录启动 `target/debug/luxd`，自动运行 migration 至 schema version 28
- 工具：SQLite `EXPLAIN QUERY PLAN`

实际输出：

```text
目录列表（library + sort）：
|--SEARCH l USING INDEX sqlite_autoindex_libraries_1 (id=?)
`--SEARCH mi USING INDEX idx_media_items_library_sort (library_id=?)

批量用户状态（user_id + item_id）：
`--SEARCH us USING INDEX sqlite_autoindex_user_item_state_1 (user_id=? AND item_id=?)

扫描游标（library_root_id + relative_path）：
`--SEARCH fe USING INDEX idx_filesystem_entries_root_path (library_root_id=? AND relative_path>?)

FTS 搜索：
|--SCAN media_search VIRTUAL TABLE INDEX 0:M5
|--SEARCH mi USING INDEX sqlite_autoindex_media_items_1 (id=?)
`--SEARCH l USING INDEX sqlite_autoindex_libraries_1 (id=?)

收藏列表（user_id + is_favorite）：
|--SEARCH user_item_state USING COVERING INDEX idx_user_item_state_favorites (user_id=? AND is_favorite=?)
|--SEARCH mi USING INDEX sqlite_autoindex_media_items_1 (id=?)
`--USE TEMP B-TREE FOR ORDER BY
```

## 推荐首页热查询（2026-08-31）

推荐的播放用户去重和收藏用户聚合已从首页请求路径移到
`recommendation_item_stats` 的每日物化刷新。首页排序只需按 `item_id` 连接该表，
再按当前用户读取一条 `user_item_state`；因此请求不再对 `playback_sessions` 和
`user_item_state` 做全库分组，也不再为每次请求计算播放用户去重。

代表性 SQLite 计划确认推荐查询使用 `recommendation_item_stats` 主键连接，图片子查询使用
`idx_item_images_recommendation_lookup` 覆盖索引。每日刷新仍会使用播放/收藏表上的
`idx_playback_sessions_recommendation_last_event`、`idx_user_item_state_recommendation_last_played`
和 `idx_user_item_state_recommendation_favorites`，但它最多在每日 02:00 批次执行一次，
不阻塞普通首页读取。

评分中位数保存在 `recommendation_rating_cache`，命中时只读取一行；持久化缓存有效期为
30 天；评分更新不会提前改变当前缓存。每日推荐 ID 保存在
`recommendation_daily_batches`，按用户、可访问媒体库范围和 UTC 02:00 批次读取，保证
同一用户在同一批次内结果稳定。

## 结论与边界

- 目录、扫描游标和批量用户状态查询均命中复合索引；FTS 查询使用 FTS5 虚拟表并通过媒体/库主键回查。
- 收藏列表先从用户收藏状态索引枚举候选，再回查媒体条目并执行可见性过滤；因此收藏数量很少时不会先扫描整个媒体库。
- Web/Emby 列表的用户状态读取使用 `Database::list_user_item_states` 分块批量查询，单块上限为 500 个 ID；媒体源、流和图片标签由目录查询联结/子查询一次取回。
- 本记录不宣称所有业务路径都没有 N+1；集合刷新、识别候选等后台流程仍需按真实数据量继续审计。
- 该结果是本机 ARM64 的查询计划证据，不代表 NAS 上的耗时、锁竞争或磁盘吞吐。
