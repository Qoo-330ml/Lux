# ADR-051：双后端迁移一致性

## 状态

已接受（2026-10-28）。落实 `docs/LUX-DATA-PATH-PLAN.md` 的 W6。

## 日期

2026-10-28

## 背景

`migrations/`（SQLite）与 `migrations-postgres/`（PostgreSQL）各自演进：SQLite 需要 FTS5 表、rowid
映射与 SQLite 触发器；PostgreSQL 需要扩展（`pg_trgm`）、语句级触发器函数与部分索引。两套目录的
文件数量已经不同（220 vs 188），版本号也不同步。没有清单时，“某次只改了一侧”只能靠人工发现。

## 决策

1. **配对清单**：`docs/migration-map.json` 记录 169 对同名迁移、51 个 SQLite 专有、19 个 PostgreSQL
   专有（每项带原因）。
2. **校验脚本**：`scripts/check-migration-parity.py` 在 `scripts/check-all.sh` 中运行，拒绝
   “未登记的新文件”“成对迁移单侧版本漂移”“登记为单侧却变成双侧（或反之）”。
3. **历史顺序分歧不再追溯**：同名迁移的相对顺序在两套历史里已经不一致（清单记录
   `rank_mismatches = 84`）。规则改为**新增迁移必须在两侧相邻**，该计数不得增长。
4. **差异必须登记原因**：新增 SQLite/PG 专有迁移时，`--write` 生成条目后必须填写原因，说明为何另一侧
   不需要；否则清单里会保留默认文案，review 时要求补。
5. **`sqlx::Any` 允许清单**：`docs/backend-branches.json` + `scripts/check-backend-branches.py`
   登记 26 个文件、265 处 `DatabaseBackend::` 分支；新文件不得带分支，已登记文件不得增长，减少需
   `--write` 记录。热点查询逐条 typed 化，不做全量替换。

## 后果

- 双后端差异从“靠人记”变成“可执行的检查”，`check-all.sh` 一行即可验证。
- 未在真实 PostgreSQL 上跑过的改动仍然需要人工判断（本机没有 PG 实例）：清单只能保证版本与理由
  齐全，不保证两侧语义等价。搜索与 FTS 维护的两后端差异因此单独记录在 `docs/COMPATIBILITY.md`。
- 迁移数量增加时清单会变长，但每次只需 `--write` 加原因，成本远低于事后排查。
