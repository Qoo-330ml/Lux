# ADR-048：同库物化读模型与 outbox

## 状态

已接受（2026-10-28）。落实 `docs/LUX-DATA-PATH-PLAN.md` 的 W2/W3/W4。

## 日期

2026-10-28

## 背景

列表、首页、继续观看与搜索原本直接读权威表（`media_items`、`media_sources`、`item_images`、
`item_aliases`），每个请求都要做多表连接、`EXISTS` 与本库可见性判断。66k 夹具上首页 p95 约 107 ms，
其中三条 rail 的查询几乎全部落在权威表上。方案评估过独立读模型库、`ATTACH` 第二库、引入 ORM 与
外部搜索服务，都因事务/备份/迁移复杂度或部署成本被否决（见计划 §7）。

## 决策

1. **同一个数据库内的派生表**：`catalog_items`、`catalog_item_stats`、`user_series_next` 与权威表同库，
   可随时 `DELETE` 后重建；权威表始终是唯一事实来源。
2. **派生数据可重建**：启动时若派生产物为空则后台重建，重建按 item id 分批、可从断点续建，重建完成
   后用 `contentSha256` 校验；重建不持长事务。
3. **水位带版本**：`catalog_items_rebuild@<READ_MODEL_SCHEMA_VERSION>`、
   `user_series_next_rebuild@<同版本>`、`search_reindex@1`。派生行形状或其输入变化时必须递增版本，
   旧水位不被信任，升级后会先重建，避免“信任旧构建写下的行”。
4. **写路径只入队**：`catalog_outbox` 是追加型队列，权威写入（`media_items`/`item_aliases` 的搜索类变更）
   只向它追加一行；消费者按水位批量应用（`search_reindex@1` 现在负责 FTS 重建）。
5. **读路径开关**：`LUX_READ_MODEL_MODE=off|shadow|on`，默认 `off`。`shadow` 比对派生页与权威页并记录
   差异；`on` 只在派生行完整时使用，缺行或未回填时整页回退权威查询，绝不返回短页。
6. **兜底**：队列缺口检测（水位 + 1 < 最老事件）触发全量重新入队；drain 失败不推进水位，重试幂等。

## 后果

- 66k 夹具实测：首页 p95 `off` 118.05 ms → `on` **16.58 ms**；继续观看 9.97 → 9.83 ms；三巡分段中位数
  `recently_added` 104.5 → 5.0 ms、`latest_by_library` 79.5 → 7.0 ms、`continue_watching` 44 → 11 ms。
  两模式响应逐字节一致。
- 索引是这项决策的代价：`catalog_items_visibility`、`catalog_items_recent_sort`、`catalog_items_latest_added`
  三个索引让 66k 库多占约 10 MB，但把计数与排序从整表扫描变成索引行走。
- 搜索在 SQLite 上最多滞后约 1 秒（队列消费者 1 s 一拍）；`FTS 空页 → LIKE 回退` 保证不会返回空结果。
  PostgreSQL 目前仍走同步维护（见 ADR-051 与 `COMPATIBILITY.md`）。
- 派生数据必须能重建，因此任何把“不可重建状态”塞进派生表的做法都被禁止；写路径也不得在请求里聚合
  全库（沿用 `docs/LUX-DEVELOPMENT.md` 的边界）。
