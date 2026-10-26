# 数据路径重构阶段报告（P0/P1）

日期：2026-10-26（本机 arm64 开发机；NAS/x86 数值不可外推）

## 结论

LUX-854–862 已实现并合并。剩余门禁项与需要项目所有者决定的事项列在末尾；P2 读模型未登记、未进入。

## 已交付

| 任务 | 提交要点 | 验证 |
|---|---|---|
| LUX-854 请求指标 | 路由模板级 count/p50/p95/p99、仅 5xx 计错、管理员端点 `GET /api/v1/admin/performance-metrics` | 单元 + 集成测试 |
| LUX-861 冷启动 | 写锁长持有告警、扫描后批量统计刷新、`ANALYZE` 后冷启动 home p95 181 ms | `scanning_jobs`、维护测试 |
| LUX-856 门禁 | Clippy 清零、共享 key 回归、10 个既有失败目标对齐 | `--all-targets` 多轮 |
| LUX-857 NFO 外置 | `media_item_nfo_cache`（SQLite 0228 / PG 0230）、读写切换、条件写、旧列写入时置空 | A/B：详情 p95 −21%，200 行更新 WAL −17%；PG 33/33 |
| LUX-858 候选减重 | 拒绝候选载荷清空、过期 PENDING 一小时宽限后有界删除 | 单元/清理回归 |
| LUX-859 保留期 | 会话/播放、账户记录、注册与迁移任务、播放历史 JSONL 归档 | 有界批次、失败保留、归档失败不删行 |
| LUX-860 元数据批写 | 页级 flush 调用 `update_media_item_nfo_metadata_batch`，整批回滚 | 批量与回滚回归 |

## 关键数字

- 端点基线（66k 夹具，release）：home p95 117.6 ms、列表 2.4 ms、搜索 67.7 ms、详情 2.0 ms、继续观看 12.1 ms。
- 冷启动 home：3 次 181.3 / 140.5 / 147.3 ms。
- NFO A/B：详情 p95 2.90 → 2.30 ms；更新 200 行 WAL 4.57 → 3.80 MB；DB 体积持平（待删旧列）。
- 诊断：无 `sqlite_stat1` 时冷启动 home 可达 90–240 s；`latest_by_library` 14 s → 2 ms（ANALYZE 后）。

## 清理实测（release，空库 + 手工旧行）

- 30 条过期 `web_sessions`、30 条超过 90 天的 `playback_history_events`，启动服务 75 秒后：
  `web_sessions=0`、`playback_history_events=0`，`archive/playback-history/*.jsonl` 共 30 行。
- 30 条过期 PENDING `metadata_candidates` 在 75 秒内未删除：候选清理挂在任务历史保留任务上
  （首次延迟 15 分钟、周期 24 小时），不是用户清理 worker；行为由单元测试覆盖，但本轮未等到周期。

## 回滚覆盖

- 元数据批写中途失败：已测整体回滚。
- 清理删除失败：已测行保留并返回错误。
- NFO 缓存回填中断、候选清理中断：**未演练**。

## 未完成 / 待决定

1. 858/859 在 66k 夹具上的清理前后行数/体积未取数。
2. 最新树扫描墙钟与补全吞吐未复测（上一轮基线为 4/8/12 worker 16.45/29.57/35.75 items/s）。
3. PostgreSQL 端点级延迟基线（同一夹具导入 PG）与扫描并发压测未做。
4. `metadata_item_nfo_cache` 旧列删除安排到下一发布周期。
5. **需要确认**：是否接受上述剩余项，批准进入 P2（读模型垂直切片）。
