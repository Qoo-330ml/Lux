# Lux 数据路径重构 · 交接说明（2026-10-28）

本文用于**在新会话里继续目标**。它只记录已经验证过的事实、当前状态、待决事项和下一步；权威规格仍是
`docs/LUX-DEVELOPMENT.md`（本文引用其段落编号不适用于其他文档）与计划工作稿
`docs/LUX-DATA-PATH-PLAN.md`（**该文件被 .gitignore 忽略，只存在于本机工作区**）。

## 0. 新会话开场提示（可直接粘贴）

> 继续 Lux 数据路径重构（W4 已完成、W5/W6 部分完成）。先读 `docs/LUX-DEVELOPMENT.md` 的
> 「数据路径重构（W0–W6）实施状态」表、W4/W5/W6 段与「门禁豁免清单」，以及
> `docs/LUX-DATA-PATH-HANDOFF.md`。工作区 `/Volumes/Toshiba/mywork/Lux`，分支 `test`，
> HEAD = `3d7daa28`（若已前进以 `git log` 为准）。W5 的“`route_p95_us` 接入后台配额”已实现（默认
> 关闭）并测过：扫描期/空闲首页 p95 比值没有下降，见 W5 段「剩余」第 4 条。下一步是 W5 的 `Fields`
> 投影，或按所有者取舍先处理该开关的去留。

## 1. 仓库与状态

- 分支 `test`，工作区干净（只有长期未跟踪的 `.pnpm-store/`、`.rustc_info.json`、`.tscli/`、
  `.vite/`、`debug/`）。
- 本阶段关键提交（时间顺序，均在 `test`）：
  `646a9147` 搜索入队 → `e96ff994` 队列缺口兜底 → `962d481f` 兼容性差异 → `9049503f` 扫描空闲退避 →
  `5b880bce` JSON gzip → `782d9eaa` ETag/304 → `da5c2550` CPU 基线 → `5461942a` 缓存策略修复 →
  `d59ce734` storage 测试适配 → `7c5a5c9d` 迁移配对清单 → `e4652465` 后端分支清单 →
  `0b71c078` ADR-048–051 → `7227b538` 测量模板/回滚门 → `d69fd8e2` W5 计划 → `7bbe6d2a` 豁免清单 →
  `ed385060` `route_p95_us` → `d979a936` 两项校验进基线 → `1fb11f07` 总览刷新 → `3d7daa28` 前台 p95 →
  后台媒体车道配额（默认关闭，A/B 未获益）。

## 2. 门禁

```
cargo test --locked --all-targets --no-fail-fast -- --test-threads=8
# → 2,246 passed / 1 failed / 76 ignored（日志示例 /tmp/final-gate-fast.log）
cargo fmt --all -- --check                     # 通过
cargo clippy --locked --all-targets --all-features -- -D warnings   # 通过
python3 scripts/check-migration-parity.py      # 169 对 / 51 SQLite 专有 / 19 PG 专有
python3 scripts/check-backend-branches.py      # 26 文件 / 265 处
python3 -m unittest tools/migration-map/test_parity.py tools/backend-branches/test_allowlist.py  # 11 项
```

唯一失败：`tests/reidentify.rs::fill_missing_dispatcher_serializes_jobs_for_one_library`
（`Error: Elapsed(())`）。**既有问题，非本次改动**：回退搜索触发器到 W4 之前仍 1/8 失败；把等待放宽到
10 s 仍 4/12 失败（12.6 s）；同目标 25/25 通过；全局许可 16/补全许可 4，不是许可饥饿。
豁免条目已写入 `docs/LUX-DEVELOPMENT.md`「门禁豁免清单」，**待所有者确认**；若选择修，另开独立任务查
“另一个库的补全任务偶发长时间不启动”。

环境注意事项（本机特有，别误判为回归）：
- 新写入的可执行脚本**首次执行**会被宿主扫描阻塞（实测 16.75 s，同文件第二次 0.12 s）。写桩脚本并立刻
  执行的测试在并行门禁下会超时；`emby_global_collection_playback` 已加“预热一次假 ffprobe”修掉。
- 并行度过高（默认线程数）会放大上述超时；`--test-threads=8` 是实测可用的上限。
- 工作树（`.agents/worktrees/*`）里跑 Web 相关测试需要 `web/dist`，否则 `web_static` 2 项 404。

## 3. 各工作包状态

| 包 | 状态 | 证据 |
|---|---|---|
| W0 观测/夹具/门禁 | 完成 | `scripts/run-api-performance.py`、`tools/catalog-fixture/`、`PERFORMANCE.md` 基线 |
| W1 生命周期与写放大 | 完成 | `LUX-DEVELOPMENT.md`「阶段 29 门」 |
| W2 读模型切片 | 完成 | 同文件 §30 与 P2 阶段门状态 |
| W3 首页/继续观看/搜索 | 完成 | LUX-867/868/869 段 + P3 完成状态；首页 p95 118.05→16.58 ms |
| W4 触发器入队与写预算 | 完成（PG 等价入队未做） | 扫描 0.42→0.37 s、WAL 11.5→11.0 MiB；触发器开销 89→13 ms；`scripts/run-scan-write-bench.py`；ADR-050 |
| W5 CPU/协议层 | 部分 | 空闲退避、gzip（首页 92,674→8,308 B）、ETag/304、CPU/请求基线（首页 277.5 / 搜索 21.3 / 列表 2.1 ms）、`route_p95_us`、前台 p95 → 媒体车道配额（默认关闭，比值 A/B 未获益）；ADR-049 |
| W6 双后端与边界 | 部分 | 迁移配对 + 后端分支两清单与校验器（已入 `AGENTS.md`）；ADR-051 |

## 4. 下一步（按序，均可独立提交）

1. ~~**W5：前台延迟反馈 → 后台并发预算**~~ 已完成（`3d7daa28`，默认关闭）。结论：接线、开关、冷却、
   真实进程验证都成立，但两轮交错 A/B 没有让“扫描期间 p95 / 空闲 p95”比值下降（`off` 2.99/3.70，
   `on` 3.36/3.64），判据未达成 → 保持默认关闭。细节与解读见 `LUX-DEVELOPMENT.md` W5「剩余」第 4 条；
   要继续压这个比值，方向应是写入突发（discovery/fingerprint）而不是媒体车道。开关去留待所有者决定。
2. **W5：`Fields` 投影**（Overview/People/Streams 按需批量补，Emby 协议层）。
3. **W5：NFO 解码 LRU**（`(item_id, fingerprint)`）。做之前先给证据说明它是热点：当前 CPU 基线显示
   列表 2.1 ms、搜索 21.3 ms 主要在 SQL（匹配/总数/排序），DTO 装配不是瓶颈（已实测：去掉搜索装配的
   深克隆后 CPU 21.3→21.8 ms，噪声内）。
4. **W6：热点查询 typed 化、存储接口拆分**（`repository.rs` 9k+ 行，按触达域拆；纯重构，先量基线）。
5. **W4：PostgreSQL 侧搜索入队**（需 PG 实例；本机没有，改动只能静态核对）。

## 5. 待决事项（需要项目所有者）

1. 门禁豁免是否接受（见 §2）。
2. `tower-http` 的 `compression-gzip` 依赖是否批准（计划 §11 第 2 条）：引入纯 Rust 的
   `async-compression` / `compression-codecs` / `compression-core`，`http-body` 转直连依赖；
   收益见 W5 段。不同意可移除，ETag/304 不受影响。
3. 搜索总数语义：若要继续压搜索 CPU（目标 −40%），需要接受“有界/近似总数”或“按相关度排序”之一，
   两者都是 API 语义变更。当前搜索 p95 106.6 ms（拉丁词元，5.6 万命中）仍在 500 ms 预算内。

## 6. 常用命令

```bash
# 读模型 off/on A/B（含扫描并发）
python3 scripts/compare-read-model-modes.py --work-dir /tmp/lux-rm-resume3 --rounds 2 --requests 10
# CPU/请求
python3 scripts/run-api-cpu-bench.py --work-dir /tmp/lux-rm-resume3 --requests 150
# 扫描写入（墙钟 / WAL / 队列排空；--legacy-search-triggers 对比 W4 之前）
python3 scripts/run-scan-write-bench.py --fixture-dir /tmp/lux-rm-resume3 --work-dir /tmp/lux-scan-bench --files 6000
# 前台 p95 反馈的日志（默认 filter 不含 lux::scan_performance，必须带 RUST_LOG）
RUST_LOG=luxd=info,lux=info LUX_BACKGROUND_FOREGROUND_THROTTLE=on <luxd>
# 媒体车道降档的扫描期/空闲 p95 比值（每臂一次，off/on 交替）
python3 scripts/probe-media-lane-ab.py --throttle off --work-dir /tmp/lux-rm-resume3
python3 scripts/probe-media-lane-ab.py --throttle on  --work-dir /tmp/lux-rm-resume3
```

66k 夹具位置：`/tmp/lux-rm-resume3/config/lux.db`（若已被清理，用 `scripts/run-api-performance.py`
重新生成；认证 cookie 为 `lux_session=lux-api-performance-token`，CSRF 为 `spec-csrf-token`）。
