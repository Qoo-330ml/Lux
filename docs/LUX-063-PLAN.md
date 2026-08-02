# LUX-063 混合库分类

## 范围

在同一媒体根内同时发现电影、剧集和无法确定的媒体。分类优先使用剧集 NFO/明确季集命名，再使用电影 NFO/明确年份；不确定项进入 `UNRESOLVED` 且保持可播放源。

## 实现

- [x] 新增 `scan_mixed_library`，递归遍历根目录并分别调用电影、剧集或 unresolved 扫描路径。
- [x] 分类优先级：SxxEyy/1x02、`tvshow.nfo`、movie NFO 根元素、明确电影年份、最后 UNRESOLVED。
- [x] UNRESOLVED 使用稳定 identity key、`PENDING` identification status 和本地 media source，不静默归为电影。
- [x] 后台扫描 job 根据媒体库 kind 选择电影/剧集/混合扫描器；混合库也支持暂停、恢复和 missing 判定。

## 验证

- 混合 fixture 同时覆盖已知电影、带 tvshow NFO 的剧集和不确定文件。
- 断言剧集父子层级不被电影/unknown 条目污染，UNRESOLVED 保留 pending 状态。
- 重扫断言 identity key 映射和 media source 稳定。

## 明确不做

- 本阶段不自动把 UNRESOLVED 重新识别为在线候选；继续使用 LUX-053/LUX-056 管理流程。
- 本阶段不实现字幕扫描和混合库专用 UI。
