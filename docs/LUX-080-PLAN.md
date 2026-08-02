# LUX-080 FTS5 搜索纵切片

## 范围

使用 SQLite FTS5 索引标题、排序标题、原标题和别名，并通过媒体库 ACL 约束结果。中文短词额外使用转义 LIKE fallback，避免默认 unicode tokenizer 无法做中文子串匹配。

## 实现

- [x] 新增 `item_aliases`、`media_search` FTS5 表及媒体/别名同步触发器。
- [x] 新增稳定排序和分页的目录搜索服务。
- [x] 新增 Lux `GET /api/v1/search?q=...`。
- [x] 新增 Emby `GET /Search/Hints?SearchTerm=...`。
- [x] 搜索结果只使用可访问媒体库，管理员和普通用户路径一致执行 ACL。

## 验证

- 中文标题、中文别名和部分中文查询 fixture 命中。
- 普通用户无库权限时结果为空，不通过搜索泄露条目。
- 全量 ARM64 回归保持通过。

## 明确不做

- 本阶段不实现复杂筛选组合、全文高亮和搜索建议缓存。
