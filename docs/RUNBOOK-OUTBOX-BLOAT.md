# 运维手册：清理膨胀的 `catalog_outbox`

适用：旧版本（含 `pdzhou/lux:test` 2026-10-30 之前的构建）因缺口误判把 `catalog_outbox` 写到数 GB 以上，磁盘紧张或
PostgreSQL 因 `no space left on device` 退出。新版本修复了成因（见 `docs/CHANGELOG.md` Unreleased），但**升级本身不会
回收已写出的空间**：`DELETE` 只把空间留给表自己复用，磁盘要靠 `TRUNCATE` 或 `VACUUM FULL` 才会归还。

## 先判断是否需要

```sql
-- PostgreSQL
SELECT pg_size_pretty(pg_total_relation_size('catalog_outbox')) AS size,
       (SELECT reltuples::bigint FROM pg_class WHERE relname = 'catalog_outbox') AS approx_rows;
```

健康部署只有千级行、MB 级体积，**不需要做任何事**。体积达 GB 级且还在增长才需要下面的步骤。

## 步骤（PostgreSQL，docker compose 部署）

1. **先腾出空间**（磁盘已满时 PostgreSQL 连 `TRUNCATE` 的 WAL 都可能写不下）：`docker builder prune -af`、清理失效容器日志。
2. 停止 Lux，保留 PostgreSQL：`docker compose stop lux`。
3. 清空队列。`catalog_outbox` 是派生数据，可由权威表重建；`TRUNCATE` 立即归还磁盘：

   ```sql
   TRUNCATE catalog_outbox;
   ```

   **不要加 `RESTART IDENTITY`**：消费者水位仍停在旧序列位置，序列回到 1 会让新事件被当作“已处理”而静默跳过。
4. 重建读模型。第 3 步丢弃了未消费的事件，`catalog_items` 可能落后于 `media_items`；只靠队列不能保证补齐
   （缺口只会在下一个新事件入队后才被发现）。清空读模型和重建水位，启动时会做一次完整重建并校验（读模型缺行期间，
   读取会整页回退到权威查询，不会返回短页）：

   ```sql
   DELETE FROM catalog_item_stats;
   DELETE FROM catalog_items;
   DELETE FROM catalog_consumers WHERE name LIKE 'catalog_items_rebuild@%';
   ```

   注意启动时的“补建”只会从已有行的最大 `item_id` 之后续建，不会重算已有行，所以必须先清空 `catalog_items`，
   单独删除重建水位不能强制全量重建。

5. 升级到修复版本镜像并启动：`docker compose pull lux && docker compose up -d lux`。启动后迁移 `0245` 会建 `search_outbox`
   并把所有条目入队一次，搜索索引随后自动收敛。
6. 验证：
   - `SELECT COUNT(*) FROM catalog_outbox;` 在重建完成后降到千级以内并保持；
   - `SELECT COUNT(*) FROM catalog_items;` 等于 `SELECT COUNT(*) FROM media_items;`；
   - Lux 日志里 `outbox gap` 至多出现零星几次 WARN，不再每秒出现；
   - 不应出现 `outbox gap on an over-capacity queue` 的 ERROR；若出现，说明队列在升级前就超过 200 万行，回到第 3 步。

## 步骤（SQLite）

停止 Lux 后：

```sql
DELETE FROM catalog_outbox;
VACUUM;
```

再执行 PostgreSQL 步骤 4 里的三条 `DELETE`。然后升级并启动，验证同上。SQLite 的 `VACUUM` 需要与数据库同等大小的临时空间。

## 不要做

- 不要 `TRUNCATE ... RESTART IDENTITY`（见步骤 3）。
- 不要在 Lux 运行时手工删除 `catalog_outbox` 的一部分行：这正是制造“缺口”的操作。
- 不要为了省事回退到旧镜像：PostgreSQL 旧二进制不会重建搜索触发器函数，回退后全文索引会停止更新。
