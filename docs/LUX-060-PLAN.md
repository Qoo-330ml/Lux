# LUX-060 剧集/季度/单集领域层级

## 范围

为 SERIES、SEASON、EPISODE 建立稳定父子关系，扫描常见 SxxEyy/1x02 命名，支持 Specials（S00）和没有 `Season N` 目录的剧集，并保证同一文件重扫不生成新逻辑 ID。

## 实现

- [x] `media_items.identity_key` 迁移与唯一索引；系列、季度和单集分别使用媒体根和相对路径生成稳定键。
- [x] 新增 `scan_series_library` 递归扫描入口，创建系列、季度、单集及单集 media source。
- [x] 支持 `S01E02`、`2x07`、`S00E03`，目录名 `Season 01`/`S01`/`Specials`，缺失季度目录时按文件标记归属。
- [x] 父子字段同时写入 `parent_id`、`series_id`、`season_number`、`episode_number`。
- [x] 保持现有电影扫描路径不变，系列扫描使用独立递归文件收集器。

## 验证

- 多季、特别篇、缺失季度目录 fixture 覆盖 2 个系列、季度和单集关系。
- 重扫断言没有新增条目/源，`identity_key -> id` 映射保持不变。
- ARM64 全量检查继续覆盖电影扫描、迁移、就绪版本和新系列扫描集成测试。

## 明确不做

- 本阶段不读取剧集 NFO、季度图片或实现 Seasons/Episodes/NextUp API；分别由 LUX-061/LUX-062 接入。
- 本阶段不处理混合库的电影/剧集分类和字幕索引。
