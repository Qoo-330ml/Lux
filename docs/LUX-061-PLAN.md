# LUX-061 剧集/季度/单集 NFO

## 范围

在 LUX-060 层级上读取剧集 `tvshow.nfo`、季度 NFO、单集旁挂 NFO，并按与电影一致的字段来源优先级更新 SERIES/SEASON/EPISODE；同时索引剧集海报、背景图和季度图片。

## 实现

- [x] `MetadataEnricher::enrich_series_library` 按稳定层级遍历系列、季度和单集。
- [x] 支持 `tvshow.nfo`、`season.nfo`/`seasonXX.nfo`、视频同名 `.nfo` 和 `episode.nfo`。
- [x] 使用本地 NFO 优先、字段级 provenance 和 metadata fingerprint；坏 NFO 不阻塞其他条目。
- [x] 索引系列 `poster`/`fanart`，季度目录图片和 `seasonXX-poster`/`seasonXX-fanart` 命名。
- [x] NFO 原子写回支持 SERIES 的 `tvshow.nfo`、SEASON 的季度 NFO 和 EPISODE 同名 NFO，并保留未知 XML。

## 验证

- 多季 fixture 覆盖剧集、季度和单集 NFO，系列/季度图片索引及二次 fingerprint 跳过。
- 写回测试确认三层目标均可更新，未知 XML 仍保留。
- 既有电影 NFO、图片和坏 NFO 测试保持通过。

## 明确不做

- 本阶段不增加 Seasons/Episodes/NextUp HTTP DTO 和用户进度查询；属于 LUX-062。
- 本阶段不处理混合库分类、字幕流和批量元数据任务。
