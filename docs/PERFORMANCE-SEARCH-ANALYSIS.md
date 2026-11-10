# 搜索查询成本分析

日期：2026-10-31。基线：`test`（含第一轮结构与性能优化）。对应交接清单
`docs/PERFORMANCE-DB-HANDOFF.md` 第 7 节最后一行“搜索：CPU 21 ms，成本在‘匹配 5.6 万行 + 计算总数 + 排序’
那条查询，不在 DTO 装配”。

本文件只记录分析与评估，不改变产品规格。已实施的改动见第 5 节；其余都是候选方案，需要迁移、
全局 PRAGMA 调整或项目所有者决策，因此**没有**实现。

> 机器：macOS arm64（Apple M4，10 核，16 GiB）。测量期间同机还有其它 worktree 的 `cargo test` /
> `clippy` 在跑（8 分钟负载均值一度到 10），**同一脚本连续两次运行可以差 20–30%**。所有 A/B 都是
> 同一夹具、同一进程内**交替执行**取中位数，只看相对方向；±5% 以内的差异按噪声处理。
> **不得**据此推断 NAS / x86_64 的耗时。

## 1. 结论摘要

1. 搜索 SQL 只有一处：`src/storage/catalog.rs` 的 `search_catalog_item_ids`（SQLite）与
   `search_catalog_item_ids_postgres`，共用的取页函数是 `fetch_catalog_search_page`。
2. 取页查询不是“COUNT 扫一遍、分页再扫一遍”：两者在同一条语句里完成，`COUNT(*) OVER()` 只多出
   **约 3–5%（11k 命中）到 19–21%（61k 命中）**。真拆成“先 COUNT 再分页”反而慢 **2.3 倍**（第 4.1 节）。
3. 真正的成本是**为每个 FTS 候选行取一次 `media_items` 行**：66k 候选里只有 11k 通过 `item_type`
   过滤，但那 66k 行都要读出来才能判断。分步测量：FTS 匹配 2.1 ms → 加 `media_items` 存在性检查
   29.3 ms → 加 `item_type` 过滤（必须取行）**91.3 ms**。类型过滤把“覆盖索引扫描”变成“逐行取表”，
   一步就从 29 ms 涨到 91 ms（第 3.2 节）。
4. `LIKE '%x%'` 用不了索引（前导通配），只能全表扫；整词搜索走 FTS5，部分匹配才回退 LIKE。
   trigram 方案已在 LUX-869 评估并否决（写入放大、1–2 字中文查询失效）。
5. 排序**必须**用临时 B-tree（`USE TEMP B-TREE FOR ORDER BY`）：`ORDER BY sort_title, id` 没有可用索引
   （`idx_media_items_library_sort` 以 `library_id` 打头），而 FTS 匹配集也不保序。
6. 已实施且不改变语义的优化：页查询改为按“匹配子查询已经读到的 `sort_title`”排序，去掉第二次
   `JOIN media_items`（SQLite 3 处、PostgreSQL 1 处），并给存储层加 `limit` 服务端上界。
   61k 命中形状 **−17%~−20%**；Lux 自己的 `MOVIE/SERIES` 形状 **−2%~−5%（噪声边缘）**（第 5 节）。
7. 交接清单里“成本在计算总数”的判断只对**命中集很大**的形状成立；对它自己记录的那条
   `GET /api/v1/search?q=Spec`（`MOVIE/SERIES`，11k 命中）来说，总数只占约 5%，成本在候选行取行。

## 2. 现状：SQL 形状与三条分支

一次搜索请求（`GET /api/v1/search`、Emby `Items?searchTerm=`）在存储层最多跑两条语句：

1. **FTS 页**（总是先跑）：

   ```sql
   SELECT mi.id, mi.sort_title AS sort_title
   FROM media_search
   JOIN media_items mi ON mi.id = media_search.item_id
   JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1 AND l.system_kind IS NULL
   WHERE media_search MATCH ? AND mi.removed_at IS NULL
     AND mi.item_type IN (?, ...)          -- 类型过滤
     AND mi.merged_into_item_id IS NULL    -- CATALOG_VISIBLE_PREDICATE
     AND (mi.has_available_source = 1 OR ...)
     [AND mi.library_id IN (?, ...)]       -- 非管理员的库 ACL
     [content restriction predicate]       -- 非管理员的家长控制
   ```

   外层取页：`SELECT matches.id[, COUNT(*) OVER() AS total] FROM (<fts>) matches ORDER BY
   matches.sort_title, matches.id LIMIT ? OFFSET ?`。

2. **回退**：FTS 页为空时，`offset = 0` 跑 LIKE 分支（标题 / 原名 / 排序名 / 别名子串，四份绑定）；
   `offset > 0` 跑 `FTS UNION LIKE`，避免“FTS 没有这一页但 LIKE 有”。

   请求路径还有一条额外语句：`search_items_with_types_and_total` 先调用
   `apply_pending_search_reindex` → `drain_search_reindex(200)`，把搜索队列里的写事件应用到 FTS
   （迁移 `0243` 之后写路径只入队）。扫描后第一次搜索最多付 200 条重建，**有界**，不在本条查询里。

`media_search` 是 FTS5 虚表（`columnsize=0`，`media_search_map` 维护 `fts_rowid ↔ item_id`），
`sort_title` 列在“与 title 只差 ASCII 大小写”时存空串，所以**不能**直接拿它做排序键——这正是原来
外层要再 JOIN 一次 `media_items` 的原因。

## 3. 夹具、EXPLAIN 与分步成本

### 3.1 夹具

`tools/catalog-fixture/generate.py` 的 `seed_spec_target`（10,000 电影 / 1,000 剧集 / 5,000 季 /
50,000 集 = 66,000 条 `media_items`），先用 `luxd` 空跑一次建库（schema 243），再种数据。
注意两点：

- 迁移 `0243` 之后 `media_items` 插入只写 `search_outbox`，种完数据要按 `drain_search_reindex`
  的同一段 SQL 重建 `media_search`，否则 FTS 是空的（我第一次量出 0 行就是这个原因）。
- 夹具里所有标题都含 `Spec`，所以 `q=Spec` 的 FTS 候选是**全库 66,000 行**，这是“常见词命中全库”
  的最坏形状，比真实库的词汇分布更极端。

两种形状分别对应：

| 形状 | `item_types` | FTS 候选 | 通过类型过滤的命中 |
|---|---|---:|---:|
| Lux Web/Emby 默认搜索 | `MOVIE, SERIES` | 66,000 | 11,000 |
| 含剧集的 Emby 搜索 | `MOVIE, SERIES, EPISODE` | 66,000 | 61,000 |

### 3.2 EXPLAIN QUERY PLAN

取页查询（`MOVIE, SERIES`）的计划：

```
CO-ROUTINE (subquery-5)
SCAN media_search VIRTUAL TABLE INDEX 0:M5
SEARCH mi USING INDEX sqlite_autoindex_media_items_1 (id=?)
CORRELATED SCALAR SUBQUERY 1           -- CATALOG_VISIBLE_PREDICATE 的可见子项
MULTI-INDEX OR
INDEX 1  SEARCH visible_child USING INDEX idx_media_items_parent_available
INDEX 2  SEARCH visible_child USING INDEX idx_media_items_series_available
CORRELATED SCALAR SUBQUERY 2           -- 合集成员的可见子项
SEARCH visible_collection USING INDEX sqlite_autoindex_collections_2 (item_id=?)
SEARCH visible_collection_item USING COVERING INDEX sqlite_autoindex_collection_items_1
SEARCH visible_child USING INDEX sqlite_autoindex_media_items_1 (id=?)
SEARCH l USING INDEX sqlite_autoindex_libraries_1 (id=?)
SCAN (subquery-5)
USE TEMP B-TREE FOR ORDER BY
```

两处关键差别（用同一 SQL 加/减一个谓词对照）：

- 只数行、不取行时是 `SEARCH mi USING COVERING INDEX sqlite_autoindex_media_items_1 (id=?)`；
  一旦需要 `item_type` / `removed_at` / `has_available_source`，就变成**非覆盖** `SEARCH mi ...`，
  每行都要回表。
- 可见性谓词里 `mi.has_available_source = 1` 在真实库里通常为真，短路后两个相关子查询不再执行
  （夹具全部为 1，实测 4→5 步只差 0.4 ms 以内）。

### 3.3 分步成本（66k 候选，`MOVIE, SERIES`，页 24 / offset 0）

| 步骤 | 中位数 |
|---|---:|
| ① FTS 匹配计数（`media_search MATCH ?`） | 2.1 ms |
| ② + JOIN `media_items`（覆盖索引，只判存在） | 29.3 ms |
| ③ + `mi.item_type IN (...)`（**必须回表**，66k 次） | 91.3 ms |
| ④ + `libraries` 过滤 | 91.6 ms |
| ⑤ + 可见性谓词 | 91.2 ms |
| ⑥ + 排序与 `LIMIT`（第二次 JOIN + 11k 行排序，**不含总数**） | 98.1 ms |
| ⑦ + `COUNT(*) OVER()`（完整取页查询） | 103.8 ms |
| 参考：只对 11k 电影按 `sort_title` 排序取 24 行 | 11.0 ms |

结论：**③ 的 62 ms 是主要成本**（66k 次回表），⑥ 的排序约 7 ms，⑦ 的总数约 6 ms。
复跑同一脚本时 ①–⑦ 的绝对值会整体漂移（例如 ③ 曾在 91–104 ms 之间、⑦ 在 100–104 ms 之间），
但“③ 最贵、④⑤ 几乎免费、总数只占几个百分点”的顺序每次都成立。

## 4. 对任务里四个问题的回答

### 4.1 “总数 COUNT 与分页是否各扫一遍”

不是。两者在同一条语句里：`COUNT(*) OVER()` 与 `ORDER BY` 共用同一趟排序（计划里只有一个
`USE TEMP B-TREE FOR ORDER BY`）。同夹具交替测量：

| 查询形状 | 带总数 | 不带总数（`count=false`） | 只 COUNT 不分页 |
|---|---:|---:|---:|
| `MOVIE, SERIES`（11k 命中） | 99.0 ms | 93.6 ms | 92.7 ms |
| `MOVIE+SERIES+EPISODE`（61k 命中） | 167.7 ms | 136.2 ms | 115.5 ms |

- 总数成本 = 5.4 ms（11k）/ 31.5 ms（61k），即命中集越大越贵。
- 反过来“先 `SELECT COUNT(*)` 再取页”在同夹具上要 **222.3 / 249.3 ms**（约 2.3 倍）：候选集被扫两遍，
  每一遍都要回表。**不要**改成两条语句。
- 把总数写成标量子查询（`(SELECT COUNT(*) FROM (<matches>) counted)`）更糟（220 ms），因为匹配子查询
  被执行两次。

### 4.2 “`LIKE '%x%'` 能否用索引”

不能。前导通配让 SQLite 无法用任何 B-tree；LIKE 分支只能扫 `media_items`（外加每行一次
`item_aliases` EXISTS）。它只在两种情况下跑：FTS 完全没有命中（`offset = 0`），或 FTS 命中少于
`offset + 1`（`UNION`）。整词搜索由 FTS5 承担；`LIKE` 只服务“部分词 / 子串”这类 FTS 词元匹配不到的查询。

可选项只有 trigram：LUX-869 已评估（同夹具建表 862 ms、库 +9.4 MB、3–6 字中文 `MATCH` 1.6–1.8 ms
对 LIKE 全表 6.5–7.8 ms），但 1–2 字查询无法命中，且每次媒体写入要多维护一份索引，本轮不采纳。

### 4.3 “排序是否需要临时 B-tree”

需要。`ORDER BY sort_title, id` 的候选集来自 FTS 匹配，任何索引都不可能同时满足“FTS 命中”和
“按 `sort_title` 有序”。现有索引 `idx_media_items_library_sort (library_id, sort_title, id)` 以
`library_id` 打头，跨库排序用不上。测量上 11k 行排序约 11 ms、61k 行约 20–30 ms，不是主要成本。

### 4.4 “能否先限制候选再 JOIN”

现状已经是“先匹配、再 JOIN `media_items` 取过滤列”。还能省的是**第二次** JOIN——它只为拿
`sort_title` 又回表一次，而匹配子查询刚刚读过这一列。这就是第 5 节实施的改动。

再往前就需要“过滤列已经和 FTS 行在一起”，即新迁移（第 6.2 节），本轮不做。

## 5. 已实施的优化

### 5.1 按匹配子查询已经读到的 `sort_title` 排序

`src/storage/catalog.rs`：

- SQLite：`fts_query`、`like_query_sql` 投影 `mi.sort_title AS sort_title`；`fts_page_query`、
  `like_page_query`、`union_page_query` 改为 `ORDER BY matches.sort_title, matches.id`，删掉
  `JOIN media_items mi ON mi.id = matches.id`。
- PostgreSQL：`like_query_sql` 同样投影并删掉外层 JOIN（与 SQLite 保持同一形状，静态核对；
  本机没有 PG 实机）。

结果不变的理由：匹配子查询的每一行来自一条 `media_items` 行，`matches.id` 唯一、`sort_title` 是它的
函数，所以排序键与 `ORDER BY mi.sort_title, mi.id` 完全一致；`UNION` 仍按 `(id, sort_title)` 去重，
而同一 id 只有一种 `sort_title`，去重结果与只按 id 去重相同。

测量（同夹具交替 20 轮取中位数）：

| 形状 | 改动前 | 改动后 | 变化 |
|---|---:|---:|---:|
| `MOVIE, SERIES`（11k 命中） | 98.5 ms | 93.7 ms | −4.9% |
| `MOVIE+SERIES+EPISODE`（61k 命中） | 143.3 ms | 117.9 ms | **−17.8%** |
| 同上，`temp_store=MEMORY` + 200 MB 页缓存 | 100.8 ms | 81.6 ms | −19.0% |

（两次独立复跑：−4.9%~−2.4% / −17.8%~−20.5%，方向一致。）

为什么 Lux 自己的形状几乎没变：第二次 JOIN 只对**通过过滤的命中行**执行（11k），而这 11k 行的页刚刚
被匹配子查询读过、还在 OS 页缓存里；61k 形状下这笔回表才明显。

端到端（release，`/api/v1/search`，预热后 p50，同机负载高，只作参考）：`q=Spec`（66k 候选）
约 182 ms，其中 SQL 约 100 ms，其余是取 24 条目录行/详情、JSON 与 gzip；`q=Spec Movie`
（10k 候选）约 25 ms，与交接清单记录的 21 ms CPU 同量级。

### 5.2 存储层的 `limit` 上界

`SEARCH_MAX_PAGE_LIMIT = 1_000` + `clamped_search_page_limit`，在 `fetch_catalog_search_page` 绑定时生效：

- 对现有调用方**没有影响**：Lux API 的分页上限是 100（`page_params`），Emby 是
  `EMBY_MAX_PAGE_LIMIT = 1_000`（`emby_page_params` 超出直接 400）。
- 作用只是把“所有列表都有服务端上界”这条规则也放到存储层，未来的调用方不会漏掉。
- `limit = 0`（Emby 的 count-only 约定）保持 0，不被放大成“一页”。

### 5.3 测试（先写、改动前后都通过）

`src/storage/repository_tests.rs`：

- `search_page_keeps_sort_title_order_and_total_across_pages`：7 条命中、含 `sort_title` 并列时按 id
  决胜，锁首页/第二页/末尾页/越界页的 id 顺序与同一个总数，以及 `include_total = false` 只影响总数。
- `search_page_union_keeps_like_only_matches_sorted_and_counted_once`：一个整词命中 + 三个只有
  LIKE/别名能命中的条目，锁 FTS-only 页、LIKE-only 页、以及 `FTS UNION LIKE` 页的顺序与“重叠行只算一次”。
- `search_page_limit_is_clamped_to_the_server_upper_bound`：上界函数的边界（0 / 负数 / 正常 / 超限 /
  `i64::MAX`）。
- 既有的 `search_pages_past_the_end_still_report_the_real_total` 覆盖越界页的总数。

## 6. 未实施的可选方案

### 6.1 连接级 PRAGMA（现场证据不足，不建议现在做）

数据库是 330 MB，而连接页缓存只有默认的约 2 MB，排序也可能落到临时文件，所以我试过只改连接 PRAGMA：

- 一次**同机有其它 worktree 满载编译**的测量显示 `cache_size = -200000` 把 11k 形状从 131 ms 降到
  68 ms、`temp_store = MEMORY` 也有 20 ms 级别的收益；
- 但在机器安静下来后，把四个组合（默认 / `temp_store=MEMORY` / 200 MB 缓存 / 两者）放在**同一进程里
  交替 15 轮**复测，三者差别落在噪声内（11k 形状 64.4 / 64.2 / 64.1 / 63.6 ms；61k 形状
  88.4 / 85.5 / 87.8 / 82.5 ms；这一组跑在比较安静的时刻，绝对值比第 3.3 节低一档，
  同样说明 PRAGMA 不是差异来源）。

也就是说前面那组的“大收益”其实是并发负载的产物，**没有可复现的证据**。**没有实现**：每个连接
200 MB 页缓存在 NAS（低内存、机械盘）上风险太大，`temp_store=MEMORY` 也让大排序失去上限，属于
全局取舍。若要重开，应在目标设备（NAS / 机械盘、真实词汇分布）上按 `docs/PERFORMANCE.md` 的 A/B
模板重测，并只做成环境变量（例如 `LUX_SQLITE_CACHE_SIZE` / `LUX_SQLITE_TEMP_STORE`），默认值不动。

### 6.2 把过滤列复制进派生搜索表（需要迁移）

现在每个候选行都要回表读 `item_type` / `library_id` / `removed_at` / `merged_into_item_id` /
`has_available_source`。若把这几列作为 `media_search` 的 `UNINDEXED` 列（或一张普通表
`media_search_state(item_id PRIMARY KEY, item_type, library_id, removed_at, has_available_source)`，
由 `search_outbox` 消费者维护），类型/库/可见性过滤就能在“不取 `media_items` 行”的情况下完成，
预计能省掉第 3.3 节 ③ 的 62 ms 中的大部分。

代价：多一份派生数据（约 66k 行 × 5 列，粗估 +5–10 MB / 66k 条），写放大增加，需要迁移 + 触发器 +
`search_reindex` 消费者同步，而且 `media_sources` 变化（`has_available_source` 翻转）也要入队。
**本轮不做**：属于新迁移，且要和 LUX-869 的 FTS 决策一起评估。

### 6.3 先取页、再计数（会改变 API 语义）

命中集很大时总数是第二贵的一项（61k 形状 31.5 ms，仅次于候选行回表）。可以改成：

- 只给“是否有下一页”而不是精确总数（前端展示会变）；
- 或总数缓存在读模型里、按查询前缀近似。

两者都改 `total` 的语义（`CatalogPage.total` 是对外 DTO），**必须**项目所有者确认，本轮不动。

### 6.4 评估后不做的小项

- **去掉 `libraries` 的 JOIN**（每行一次库 PK 探测）：实测只占 0.3 ms，却要先把“启用且非系统库”的
  id 列表查出来再做 `IN (...)`，引入库状态变化与请求之间的竞态，收益不值。
- **`ORDER BY matches.id`**：能省排序（61k 形状 124.7 ms），但会改变结果顺序，违反“不改搜索语义”。
- **`media_search_map` 加 `item_rowid` 走 `media_items.rowid`**：能省掉每行的 TEXT 主键索引探测，
  但需要迁移，且探测本身只占 29 ms 里的一部分（大部分是回表）。

## 7. 剩余风险与后续建议

1. **PostgreSQL 只做静态核对**。本机没有 PG，形状与 SQLite 对齐但没有跑过
   `cargo test --locked --test postgres_database -- --ignored`。上线前应在真实 PG 上复跑第 5.3 节的
   语义测试（PG 的 `media_search` 是普通表，`ILIKE` 分支行数可能不同）。
2. **测量环境有并发负载**（同机其它 worktree 的 `cargo test` / `clippy`），同一脚本两次运行可以差
   20–30%，绝对毫秒数偏高；A/B 靠交替执行和中位数，但 ±5% 内的差异不可信。要写进
   `docs/PERFORMANCE.md` 的正式数字应在空闲机器上用 `scripts/run-api-cpu-bench.py` 重跑。
3. **夹具是“常见词命中全库”的最坏形状**，真实库的候选行数通常远小于 66k；因此本次改动在真实库里的
   收益应低于 61k 形状的 −18%，接近 Lux 形状的 −5%（含剧集的 Emby 搜索受益最明显）。
4. **`drain_search_reindex(200)` 仍在请求路径里**：扫描/刮削后第一次搜索最多付 200 条 FTS 重建。
   有界、设计如此，但它意味着“搜索请求成本”在扫描后不稳定，做 A/B 时要先确
   认日志里没有 `search reindex applied`。
5. **`limit = 0` 的既有语义问题（未修）**：Emby 的 count-only 请求（`Limit=0&EnableTotalRecordCount=true`）
   走 `search_catalog_item_ids` 时，页查询 `LIMIT 0` 返回空页，`COUNT(*) OVER()` 没有行可读，总数也被
   报成 0。这是与本次优化无关的既有行为，修它会改变可见结果，需要单独任务。
6. 夹具 FTS 由 `drain_search_reindex` 的同一段 SQL 重建；若将来修改重建语句（例如加过滤列），
   夹具脚本与本文件的分步数字都要跟着更新。

## 8. 复现

夹具与分析脚本放在本 worktree 的未跟踪目录 `.analysis/`（不提交）。复现步骤：

1. `cargo build --locked`；用 `LUX_CONFIG_DIR=<work>/config luxd` 空跑一次建库，停掉；在 config 目录写
   `{"backend":"SQLITE"}` 到 `database.json` 并删除 `.database-selection-required`（否则进程只提供
   setup 路由，业务路由 404）。
2. `tools/catalog-fixture/generate.py` 的 `seed_spec_target(<db>, <media>, session_token=...)`；
   然后按 `src/storage/search_reindex.rs` 的 `reindex_search_items` 语句重建 `media_search`，
   清空 `search_outbox`，`ANALYZE`，`PRAGMA wal_checkpoint(TRUNCATE)`。
3. `EXPLAIN QUERY PLAN` 与分步计时用 `sqlite3` / python `sqlite3` 直接跑第 2 节的 SQL。
4. 端到端用 `scripts/run-api-cpu-bench.py --work-dir <work> --binary target/release/luxd`
   （注意该脚本目前不处理第 1 步的 `database.json`，需要先手工补上）。
