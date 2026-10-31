# media_items 索引与触发器审计（第二轮 W4）

范围：`migrations/`（SQLite 与 PostgreSQL 配对）、`src/` 内对 `media_items` 的只读查询归因、本文件，
以及证据脚本 `scripts/audit-media-items-indexes.py`。对应交接清单 `docs/PERFORMANCE-DB-HANDOFF.md`
第 5 节 W4「`media_items` 写放大：约 18 个索引、6 个触发器」。

审计日期：2026-11-02。基线：`test` 分支，迁移序列到 `0241`（SQLite）/ `0243`（PostgreSQL）。
本机：macOS `arm64`。**本文件里的查询计划与数字只描述这台开发机，不能外推到 NAS（机械盘 / 低功耗 x86）。**

## 1. 结论摘要

1. **索引数量对不上**：从迁移链建库后 `media_items` 有 **17 个显式索引**（另有 1 个主键 autoindex），
   而新建库经 `Database::connect` 后是 **20 个**。原因见第 6 节：旧的 catalog 修复路径会重建
   `media_items` 并执行一份**硬编码索引清单**，那份清单既不等于迁移链，也会把已删除的索引写回来。
   交接清单里「约 18 个索引」是迁移链的数；真实新建库是 20 个。
2. **触发器是 13 个，不是 6 个**：其中 3 个（含 2 个 `catalog_outbox` 入队与 1 个 provider 表同步）在每次
   `media_items` 写入时都会动，`media_items_catalog_au` 更是**任意列的 UPDATE 都会触发**。见第 7 节。
3. **本轮删除了 2 个有明确证据冗余的索引**（迁移 `0242` / PG `0244`）：
   `idx_media_items_parent_removed`、`idx_media_items_series_removed`。二者都是更宽的 `*_available`
   索引的严格左前缀（第 5 节有双向的查询计划证据）。
4. **修好了 3 个被重建路径吞掉的索引**：`idx_media_items_removed_at`（迁移 0168）、
   `idx_media_items_series_episode_latest` 与 `idx_media_items_parent_episode_latest`（迁移 0215）
   在重建路径里缺失，导致**新建库从来没有**这三个索引。首页「最近添加」的 `MAX(added_at)` 与
   清理任务的 `removed_at` 查询在新建库上因此退化为全表/大范围扫描。
5. **另有 3 个索引被重建路径复活、需要项目所有者决定**（不是我这个任务能单方面删的，原因见第 6.3 节）：
   `idx_media_items_library_added_visible`（0220 已删）、`idx_media_items_library_premiere_sort`、
   `idx_media_items_library_rating_sort`（0221 已删）。
6. **证据不足、留待 NAS 实测 dbstat 的**：`idx_media_items_nfo_collection_reference_pending`、
   `idx_media_items_updated_at`、`idx_media_items_merged_into`（还有一个可选项：把
   `merged_into_item_id` 改成 PG 0182 那样的 partial 索引）。第 8 节列出各自的实测方法。
7. **没有删除任何触发器**：13 个都有实际写入方，其中 12 个是功能性派生数据同步，1 个
   （`media_items_updated_at_ai`）在迁移后的表默认值下基本不触发。它们才是 `media_items` 写放大
   的主项，比索引更值得下一轮处理，但都涉及可见行为（读模型/搜索新鲜度），需要单独确认。

## 2. 复现方法

### 2.1 索引使用归因：`scripts/audit-media-items-indexes.py`

```bash
python3 scripts/audit-media-items-indexes.py
python3 scripts/audit-media-items-indexes.py --simulate-drop idx_media_items_parent_available
```

脚本做四件事：

1. 按版本号顺序把 `migrations/*.sql` 应用到临时 SQLite 库（与 `sqlx::migrate!("./migrations")` 相同的文件与顺序）；
2. 灌一个**规模与形状接近真实库**的数据集（6 000 部电影 + 60 个系列 × 4 季 × 12 集，混合
   「可用/不可用/已删除/已合并」行），然后 `ANALYZE`。表太小会误导计划器，脚本的注释记录了这一点；
3. 对 `CORPUS` 里每条代表性查询执行 `EXPLAIN QUERY PLAN`，统计每个索引被哪些查询选中；
4. 报告「没有任何查询使用」的索引，以及「列清单是另一个索引左前缀」的结构性冗余；
   加 `--simulate-drop <索引>` 会在灌完数据后删掉该索引再重跑全部计划，用来看哪个索引接手。

`CORPUS` 的每条都带 `source`（Rust 文件）与 `anchor`（必须仍出现在该文件里的 SQL 片段）。
源码改动导致片段消失时脚本直接失败，避免文档继续描述已经不存在的 SQL。

### 2.2 真实体积：dbstat

`media_items` 上每个索引的字节数用 SQLite 的 `dbstat` 虚表测。两条等价路径：

- **应用内**：管理员端点 `GET /api/v1/admin/database-diagnostics`（`src/storage/database_diagnostics.rs`
  的 `DatabaseDiagnostics`）在 `include_relation_sizes` 时按对象名聚合 `dbstat`，返回
  `relations[].size.indexBytes` 与 `indexes[]` 的逐索引 `bytes/pages`，并给出
  `relationSizesAvailable` 表示当前 SQLite 构建是否带 `SQLITE_ENABLE_DBSTAT_VTAB`。
- **离线**（推荐，避免在服务里跑 90 秒的 dbstat）：停服后对数据库副本执行

  ```bash
  sqlite3 /path/to/copy.db \
    "SELECT name, SUM(pgsize) AS bytes, COUNT(*) AS pages
       FROM dbstat WHERE name LIKE 'idx_media_items_%'
      GROUP BY name ORDER BY bytes DESC;"
  ```

  `sqlite3` CLI 需要带 dbstat 虚表；macOS 自带的 `/usr/bin/sqlite3` 与本仓库构建的 SQLite 都满足
  （构建产物是否带 dbstat 以端点返回的 `relationSizesAvailable` 为准）。

### 2.3 写预算：前后对比

```bash
python3 scripts/run-scan-write-bench.py --fixture-dir /tmp/lux-fixture --files 6000 \
    --work-dir /tmp/lux-w4-before
python3 scripts/run-scan-write-bench.py --fixture-dir /tmp/lux-fixture --files 6000 \
    --work-dir /tmp/lux-w4-after
```

它报告一次全库扫描的墙钟、WAL 增长与数据库增长。**必须先跑 before 再做改动**；本轮只有索引数量
变化（少 2 个、新建库多回 3 个），单次扫描的差异预计落在噪声内，所以本文件不给数字，只给方法和
判定标准：在 NAS 上如果 before/after 的 WAL 字节差 < 5%，说明这批索引不是写放大的主项，应该转去
看第 7 节的触发器和交接清单 W3（UPDATE 保护）。

### 2.4 集成测试

```bash
cargo test --locked --test storage index_audit::
```

固定三件事：

1. 从空库 `Database::connect` 之后 `media_items` 的完整索引清单（防止重建路径再次偏离）；
2. 被删索引对应的关键查询（`count_catalog_children`、`list_series_episode_ids`、
   `sync_played_container_states_for_scope`）仍然命中 `media_items` 上的索引、不退化为全表扫描；
3. 用 `INDEXED BY` 强制计划，证明存活的 `*_available` 索引可以完整回答被删索引的谓词
   （不需要临时排序、不需要回表扫描）。这一条不依赖计划器启发式，因此不受 SQLite 版本影响。

## 3. 索引清单与结论

「生效于」一列区分两种库：**迁移链** = 从迁移建库（升级安装接近这个集合）；**新建库** =
`Database::connect` 之后（会经过第 6 节的 catalog 修复）。列「使用方」给出函数名，行号已过期。

| 索引（列 / 谓词） | 使用方 | 结论 |
|---|---|---|
| `idx_media_items_identity_key` UNIQUE `(identity_key)` WHERE NOT NULL | `ensure_global_generated_media_item`、`update_local_provider_ids_for_identity_if_empty`、全局合集同步按 provider id 找工作 | 保留（唯一性，不能删） |
| `idx_media_items_library_sort` `(library_id, sort_title, id)` | `list_catalog_rows` 的默认目录分页、`list_resume_items_with_next_episode` 等按 `sort_title` 排序的分页 | 保留（默认排序） |
| `idx_media_items_library_name_sort` `(library_id, LOWER(title), id)` WHERE `removed_at IS NULL` | `CatalogSort::Name`（`catalog_item_order`） | 保留（排序专用） |
| `idx_media_items_library_sortname_sort` `(library_id, LOWER(sort_title), id)` WHERE `removed_at IS NULL` | `CatalogSort::SortName`（`catalog_item_order`） | 保留（排序专用） |
| `idx_media_items_library_added_sort` `(library_id, added_at DESC, LOWER(title), id)` WHERE `removed_at IS NULL` | `CatalogSort::DateCreated`（`catalog_item_order`）、`count_catalog_items` | 保留（排序专用） |
| `idx_media_items_library_type_visible` `(library_id, item_type, id)` WHERE `removed_at IS NULL` | `count_catalog_root_items_by_library` | 保留 |
| `idx_media_items_people_visible` `(library_id, id)` WHERE `removed_at IS NULL` | `list_person_index_item_ids`（需要按 `id` 顺序增量扫描）；`CatalogSort::PremiereDate`/`Rating` 也拿它做 `library_id` 驱动 | 保留 |
| `idx_media_items_parent_available` `(parent_id, removed_at, has_available_source)` | `count_catalog_children`、`sync_played_container_states_for_scope`、`list_resume_items_with_next_episode` | 保留（删掉 `parent_removed` 后的唯一 parent 索引；具体计划在版本间有差异，见 §5） |
| `idx_media_items_series_available` `(series_id, removed_at, has_available_source)` | `list_series_episode_ids`、`write_user_series_next_pointer` | 保留（删掉 `series_removed` 后的唯一 series 索引） |
| `idx_media_items_parent_episode_latest` `(parent_id, added_at)` WHERE EPISODE + 可用 | 首页最近添加的 `MAX(added_at)` parent 分支 | 保留；**新建库原本缺失**，本轮补进重建清单 |
| `idx_media_items_series_episode_latest` `(series_id, added_at)` WHERE EPISODE + 可用 | 首页最近添加的 `MAX(added_at)` series 分支 | 保留；**新建库原本缺失**，本轮补进重建清单 |
| `idx_media_items_home_unavailable_series` `(library_id, added_at DESC, sort_title, id)` WHERE `removed_at IS NULL AND item_type = 'SERIES' AND has_available_source = 0` | `sqlite_recent_catalog_rows_by_library_query` 的 `unavailable_series` 分支 | 保留（谓词与查询完全对齐） |
| `idx_media_items_merged_into` `(merged_into_item_id)` | 没有 SELECT 单独按这一列过滤；唯一用途是 `merged_into_item_id … REFERENCES media_items(id) ON DELETE SET NULL` 的**外键子表探测**（删除父行时 SQLite 需要按子键查子表） | 保留（外键仍需要它）；是否改 partial 见第 8.3 节 |
| `idx_media_items_migration_title` `(item_type, sort_title, production_year, library_id, id)` WHERE `removed_at IS NULL` | `list_migration_media_identity_candidates_filtered`（Emby 迁移的标题回退匹配，功能仍在用） | 保留 |
| `idx_media_items_nfo_collection_reference_pending` `(id)` WHERE MOVIE + 未删除 + `nfo_collection_reference_key IS NULL` | `list_pending_movie_nfo_collection_reference_reconciliations` | 保留，**待 NAS 实测**（第 8.1 节：小样本下计划器改用 `migration_title`） |
| `idx_media_items_removed_at` `(removed_at)` WHERE `removed_at IS NOT NULL` | `database_cleanup` 的保留期清理、`watch_state_archive` | 保留；**新建库原本缺失**，本轮补进重建清单 |
| `idx_media_items_updated_at` `(updated_at, id)` | 只有一条可能的消费者：目录过滤 `min_date_last_saved`（`build_catalog_filter` 的 ` AND mi.updated_at >= ?`，Emby 增量刷新） | 保留，**待 NAS 实测**（第 8.2 节：范围不够窄时计划器选 `library_sort`） |

以下 3 个只在**新建库**存在（升级安装被 0220/0221 删掉了），因此单独列出：

| 索引 | 迁移状态 | 结论 |
|---|---|---|
| `idx_media_items_library_added_visible` `(library_id, added_at DESC, sort_title, id)` WHERE 可用 | 0220 已 `DROP` | 复活项，**待所有者决定**（第 6.3 节） |
| `idx_media_items_library_premiere_sort` | 0221 已 `DROP` | 复活项，**待所有者决定** |
| `idx_media_items_library_rating_sort` | 0221 已 `DROP` | 复活项，**待所有者决定** |

## 4. 已执行的变更

| 文件 | 内容 |
|---|---|
| `migrations/0242_drop_redundant_media_items_indexes.sql` | `DROP INDEX IF EXISTS idx_media_items_parent_removed;` / `… series_removed;` |
| `migrations-postgres/0244_drop_redundant_media_items_indexes.sql` | PostgreSQL 配对（同一语义、同样注释） |
| `src/storage/migration.rs` | catalog 修复路径的硬编码索引清单：删掉上面 2 个；补上 0168 的 `removed_at` 与 0215 的 2 个 episode-latest partial 索引 |
| `tests/storage/index_audit.rs`（新增） | 新建库索引清单 + 被删索引对应查询的计划断言 |
| `tests/scanner/scanner.rs` | `media_catalog_migration_creates_expected_tables` 的索引期望从 `parent_removed`/`series_removed` 改为 `parent_available`/`series_available`（该测试断言的是修复路径产出的 schema，索引改名属于同一变更） |
| `scripts/audit-media-items-indexes.py`（新增） | 第 2.1 节的证据脚本 |

## 5. 删除索引的证据（`parent_removed` / `series_removed`）

结构：

```text
idx_media_items_parent_removed (parent_id, removed_at)        左前缀 ⊆ idx_media_items_parent_available (parent_id, removed_at, has_available_source)
idx_media_items_series_removed (series_id, removed_at)        左前缀 ⊆ idx_media_items_series_available (series_id, removed_at, has_available_source)
```

两个 `*_available` 索引都**不是 partial 索引**，所以窄索引能服务的任何 `parent_id`/`series_id`
等值与 `removed_at` 等值/范围谓词，宽索引都能服务；宽索引还多一个 `has_available_source`
等值谓词可用。两者都没有唯一性，也没有「排序专用」作用（列顺序相同、方向相同），所以窄索引
既不能被唯一约束需要，也不能提供宽索引提供不了的顺序。

双向的查询计划证据（本机 arm64，脚本的默认数据集：6 000 部电影 + 2 880 集）：

- 在宽索引存在时，`count_catalog_children`（`WHERE parent_id = ? AND item_type IN (…) AND removed_at IS NULL`）
  与 `sync_played_container_states_for_scope`（`parent_id = ? AND removed_at IS NULL AND has_available_source = 1`）
  **都选择宽索引**，没有一个选窄索引：
  `SEARCH mi USING INDEX idx_media_items_parent_available (parent_id=? AND removed_at=?)`、
  `SEARCH episode USING INDEX idx_media_items_parent_available (parent_id=? AND removed_at=? AND has_available_source=?)`。
- 反过来 `--simulate-drop idx_media_items_parent_available`（在 0242 合入**之前**测得；现在窄索引
  已经不存在，要复现需先在临时库里手工重建 `idx_media_items_parent_removed`），上述两条计划**全部
  转用窄索引**且没有变成全表扫描；`idx_media_items_series_available` 与 `series_removed` 之间也是
  同样的互换关系。

也就是说：删掉窄索引后，所有现存查询都由宽索引接手，行检查数不变，而每次 `media_items` 插入/更新
少维护一个 B-tree。删索引走迁移是必须的，`python3 scripts/check-migration-parity.py` 保证 PG 侧同步。

**版本与数据相关的细节**（不影响上面的结论，但会影响你看到的具体计划）：`count_catalog_children`
带 `JOIN libraries` 时，同一份数据在 SQLite 3.54（Python 模块）上选 `parent_available`，而在
SQLite 3.51（sqlx 内置）上曾选 `idx_media_items_migration_title (item_type=?)` 再过滤。两者都不是
全表扫描，且窄索引在两种情况下都没有被选中——这正是删除它的理由，但**不要**把「某个查询一定走
某个索引」当成跨版本的断言，集成测试因此只断言「仍在 `media_items` 的索引上、没有全表扫描」，
另加一条 `INDEXED BY` 的可替代性断言。

## 6. 重建路径（legacy catalog 修复）与迁移链的分歧

### 6.1 机制

`src/storage/migrate_sqlite_catalog_constraints` 在迁移之后运行：只要 `media_items` 表定义里还有
`UNIQUE (library_id, sort_title, production_year)`（`migrations/0006_media_catalog.sql` 就有，
后续没有任何迁移删掉它），它就把整张表重建为无该约束的新表，并**执行一份写死在 Rust 里的索引清单**。
迁移链上所有对 `media_items` 索引的增删都在它之前发生，所以清单是最终状态。

### 6.2 本轮已修：3 个索引被吞掉

重建清单漏了迁移链后来加的 3 个索引，导致**新建库从来没有它们**（升级安装因为迁移已经建好，反而有）：

- `idx_media_items_removed_at`（0168）：清理/归档任务靠它按 `removed_at` 找过期软删行；
- `idx_media_items_series_episode_latest`、`idx_media_items_parent_episode_latest`（0215）：
  首页「最近添加」的 `MAX(added_at)` 靠它们做 index-only 扫描，缺了就会退化成范围扫描。

已补进 `src/storage/migration.rs` 的重建清单，并由 `tests/storage/index_audit.rs` 的
`EXPECTED_MEDIA_ITEM_INDEXES` 固定下来。

### 6.3 未修：3 个索引被复活（需要所有者决定）

`0220_drop_unused_home_catalog_index.sql` 与 `0221_drop_unused_catalog_sort_indexes.sql` 删掉了
`library_added_visible`、`library_premiere_sort`、`library_rating_sort`，但重建清单仍在创建它们，
于是**新建库会带着 3 个迁移认为已删除的索引**。本任务没有单方面改，原因是有既有断言依赖现状：

- `tests/storage/storage.rs` 的 `assert_homevideos_video_types_are_accepted` 断言新建库
  「被保留的索引」数量为 5，其中包含 `library_added_visible`；
- `src/storage/repository_tests.rs` 的排序计划测试把 `PremiereDate`/`Rating` 期望为使用
  `library_premiere_sort`/`library_rating_sort`，而 `docs/PERFORMANCE.md` 也把这两个索引记录为
  「排序不再出现 TEMP B-TREE」的依据。

要收敛这一项，需要所有者同时确认：删除 `tests/storage/storage.rs` 的期望、改写
`src/storage/repository_tests.rs` 的 2 个排序用例、并更新 `docs/PERFORMANCE.md` 的结论。
在那之前，新建库比升级安装多 3 个索引（每次 `media_items` 写入多 3 次索引维护）。

## 7. 触发器（13 个）与写放大

`SELECT name FROM sqlite_master WHERE type = 'trigger' AND tbl_name = 'media_items'` 返回 13 个：

| 触发器 | 触发时机 | 每次触发的写入 | 判断 |
|---|---|---|---|
| `media_items_catalog_ai` | AFTER INSERT | `catalog_outbox` 最多 3 行（自身、parent、series） | 需要；派生读模型 |
| `media_items_catalog_au` | **AFTER UPDATE（无列清单）** | `catalog_outbox` 最多 5 行 | 保留但最贵：任意列 UPDATE 都会触发 |
| `media_items_catalog_ad` | AFTER DELETE | `catalog_outbox` 最多 3 行 | 需要 |
| `media_items_search_ai` | AFTER INSERT | `catalog_outbox` 1 行（kind='search'） | 需要（0237 已改成入队） |
| `media_items_search_au` | AFTER UPDATE OF title/sort_title/original_title + `WHEN … IS NOT …` | 1 行 | 需要；只在标题真的变化时触发，写法正确 |
| `media_items_search_ad` | AFTER DELETE | 1 行 | 需要 |
| `media_item_provider_ids_ai` | AFTER INSERT | `json_each` 解析 + N 行 upsert | 需要（provider 索引表） |
| `media_item_provider_ids_au` | AFTER UPDATE OF item_type, provider_ids_json | 先 DELETE 再 N 行 | 需要；有列清单，范围已收窄 |
| `media_items_updated_at_ai` | AFTER INSERT WHEN `updated_at = 0` | 一次额外 UPDATE，会**再次触发 `media_items_catalog_au`** | 迁移后表默认值是 `unixepoch()`，只有显式写 0 的调用方才会付这份钱 |
| `trg_media_items_content_restriction_index_queue_insert` | AFTER INSERT | 1 行 upsert | 需要 |
| `trg_media_items_content_restriction_index_invalidate_nfo` | AFTER UPDATE OF nfo_metadata_json + `WHEN … IS NOT …` | 1 DELETE + 1 upsert | 需要；有列清单 + WHEN 保护 |
| `collection_media_item_sync_au` | AFTER UPDATE OF removed_at, has_available_source, library_id, item_type + WHEN | `collection_sync_outbox` 每组 1 行 | 需要；列清单正确 |
| `collection_media_item_sync_bd` | BEFORE DELETE | 1 行/组 | 需要 |

结论：13 个都有人用，删任何一个都会丢派生数据，本轮不动。真正的写放大来源是
`media_items_catalog_au`（无列清单/无 WHEN 的整行 UPDATE 触发），一次元数据更新可以顺手产生
最多 5 行 `catalog_outbox`。收紧它（列清单或 WHEN 保护）属于改变读模型新鲜度语义，需要单独确认；
这与交接清单 W3（UPDATE 前先比值的保护）是同一个方向的工作。

## 8. 待 NAS 实测 dbstat 后决定（本轮不删）

### 8.1 `idx_media_items_nfo_collection_reference_pending`

- 设计意图（0240）：全局合集对账轮询 `list_pending_movie_nfo_collection_reference_reconciliations`
  只取少量 pending 电影，靠 `(id)` 上的 partial 索引按 `id` 顺序取行、不排序。
- 本机实测：当 pending 只占 12/6 000 行时，计划器**仍然**选择
  `SEARCH media_items USING INDEX idx_media_items_migration_title (item_type=?)` + `USE TEMP B-TREE FOR ORDER BY`；
  用 `INDEXED BY` 强制时才会走 pending 索引。也就是说这条索引在这台机器的这个形状下没有被选中，
  但计划依赖数据分布与 SQLite 版本，证据不足以删除。
- NAS 上怎么测：取生产库副本，`ANALYZE` 后执行

  ```bash
  sqlite3 copy.db "EXPLAIN QUERY PLAN <PENDING_NFO_COLLECTION_RECONCILIATION_QUERY 的 SQL>"
  sqlite3 copy.db "SELECT COUNT(*) FROM media_items WHERE item_type='MOVIE' AND removed_at IS NULL AND nfo_collection_reference_key IS NULL;"
  ```

  若计划连续两次（升级后、一轮对账后）都不使用该索引，且 dbstat 显示它占用可观字节，再删除。

### 8.2 `idx_media_items_updated_at`

- 唯一潜在使用方是 `build_catalog_filter` 里的 ` AND mi.updated_at >= ?`（Emby 增量刷新）。
- 本机实测：范围较宽（44% 行）时选 `idx_media_items_library_sort`（避免 TEMP B-TREE）；范围极窄时
  也仍然选 `library_sort`，因为目录过滤总是带 `ORDER BY`+`LIMIT`。先前在 20 000 行的纯电影库上
  曾观察到 `SEARCH mi USING INDEX idx_media_items_updated_at (updated_at>?)` + TEMP B-TREE。
  结论：**数据相关**，不能凭本机判断。
- NAS 上怎么测：在真实库副本上分别用「从 5 分钟前」「从 24 小时前」两档 `min_date_last_saved` 生成
  目录过滤 SQL，`EXPLAIN QUERY PLAN` 看是否选中；同时用 dbstat 记下该索引字节。若两档都不选中，
  可以删除并保留 `updated_at` 的过滤条件（仍然正确，只是走别的索引）。

### 8.3 `idx_media_items_merged_into`（可选：改 partial）

- 它没有 SELECT 消费者，只服务 `merged_into_item_id` 外键的 `ON DELETE SET NULL` 子表探测。
- PostgreSQL 侧 0182 已把它改成 partial（`WHERE merged_into_item_id IS NOT NULL`），SQLite 侧
  `migrations/0120_manual_item_merges.sql` 仍是全表索引，且重建清单里也是全表。若 NAS 的 dbstat 显示
  它体积可观，可以照 PG 的样子改 partial —— 但 SQLite 只会在查询里能推出 `IS NOT NULL` 时才用 partial
  索引，外键探测是否满足这一点需要在本机先做一次 `EXPLAIN` 验证（本轮未验证），所以只记录、不实施。

## 9. 未验证与剩余风险

- 全部查询计划来自本机 arm64 + SQLite（Python `sqlite3` 模块内置版本）；`Database::connect` 用的
  SQLite 构建可能版本不同，计划选择的经验值不能替代 NAS 实测。
- PostgreSQL 侧只做了静态等价性核对：本机没有 PG 实例，未运行
  `cargo test --locked --test postgres_database -- --ignored`。PG 也没有 SQLite 那样的逐索引扫描计数，
  §8 的三项在 PG 上只能靠 `pg_stat_user_indexes.idx_scan` 观察。
- `scripts/audit-media-items-indexes.py` 用迁移链建库，**不模拟**第 6 节的重建路径（那是 Rust 逻辑）。
  新建库最终 schema 由 `tests/storage/index_audit.rs` 的清单断言覆盖。
- 数据集是合成的：真实库的行数、`has_available_source`/`removed_at` 分布、`nfo_collection_reference_key`
  空值比例都会改变计划器选择。脚本把数据规模写成常量，是为了让结论可复现，不代表生产分布。
- 第 6.3 节的「新建库多 3 个索引」仍未收敛：这是本轮明确的遗留项，需要所有者决定后单独做。
- 删掉 `parent_removed`/`series_removed` 后，`tests/scanner/scanner.rs` 的 schema 期望已同步更新；
  如果其他分支也在改同一个测试函数的期望，合并时需要按索引名为准重新对齐。
