# Lux 数据路径重构 · 交接说明（2026-10-28）

本文用于**在新会话里继续目标**。它只记录已经验证过的事实、当前状态、待决事项和下一步；权威规格仍是
`docs/LUX-DEVELOPMENT.md`（本文引用其段落编号不适用于其他文档）与计划工作稿
`docs/LUX-DATA-PATH-PLAN.md`（**该文件被 .gitignore 忽略，只存在于本机工作区**）。

## 0. 新会话开场提示（可直接粘贴）

> 继续 Lux 数据路径重构（W4 已完成、W5/W6 部分完成）。先读 `docs/LUX-DEVELOPMENT.md` 的
> 「数据路径重构（W0–W6）实施状态」表、W4/W5/W6 段与「门禁豁免清单」，以及
> `docs/LUX-DATA-PATH-HANDOFF.md`。工作区 `/Volumes/Toshiba/mywork/Lux`，分支 `test`，
> HEAD = `d0b1b9a8`（若已前进以 `git log` 为准）。W5/W6 的剩余项都已落地或有书面结论；剩下的是待所有者
> 拍板的产品决定（前台降档默认值、门禁豁免）与“PG 侧写预算基准”的证据缺口；gzip 依赖与搜索总数语义已于
> 2026-10-29 决定（见 §5）。
> W5：前台 p95 → 媒体车道的降档已实现（默认关闭）并验证（媒体车道有活时比值 2.55 → 1.91、恢复 2.5 s），
> 30 s 时间窗 + 按读模型模式阈值；`Fields` 列表 `MediaStreams`、NFO 解码缓存（−0.28…−0.36 ms/重复详情，
> 指纹+bytes+updated_at 校验）、People 与 DTO 成本均已量。W6：迁移/分支两份清单与校验器、存储拆分两片
> （`repository.rs` 9,222 → 5,948 行）、typed 化 4 个热点查询；其余 `row.get` 有书面理由暂缓。

## 1. 仓库与状态

- 分支 `test`，工作区干净（只有长期未跟踪的 `.pnpm-store/`、`.rustc_info.json`、`.tscli/`、
  `.vite/`、`debug/`）。
- 本阶段关键提交（时间顺序，均在 `test`）：
  `646a9147` 搜索入队 → `e96ff994` 队列缺口兜底 → `962d481f` 兼容性差异 → `9049503f` 扫描空闲退避 →
  `5b880bce` JSON gzip → `782d9eaa` ETag/304 → `da5c2550` CPU 基线 → `5461942a` 缓存策略修复 →
  `d59ce734` storage 测试适配 → `7c5a5c9d` 迁移配对清单 → `e4652465` 后端分支清单 →
  `0b71c078` ADR-048–051 → `7227b538` 测量模板/回滚门 → `d69fd8e2` W5 计划 → `7bbe6d2a` 豁免清单 →
  `ed385060` `route_p95_us` → `d979a936` 两项校验进基线 → `1fb11f07` 总览刷新 → `3d7daa28` 前台 p95 →
  媒体车道配额 → `76f23774` 真实媒体负载验证 → `dbd228e0` 列表 `MediaStreams` → `d15c55af` 30 s 时间窗 +
  模式阈值 → `fe8a2f68` NFO 成本、`adf0986c` People 成本 → `98819ae9` NFO 解码缓存 → `4da06159` DTO 深克隆
  测量 → `0025f08c` 邮件 outbox 域拆分。

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
| W4 触发器入队与写预算 | 完成（两后端入队；PG 侧无写预算基准） | 扫描 0.42→0.37 s、WAL 11.5→11.0 MiB；触发器开销 89→13 ms；`scripts/run-scan-write-bench.py`；ADR-050 |
| W5 CPU/协议层 | 部分（仅默认值待定） | 空闲退避、gzip（首页 92,674→8,308 B）、ETag/304、CPU/请求基线（首页 277.5 / 搜索 21.3 / 列表 2.1 ms）、前台 p95 → 媒体车道配额（默认关闭；比值 2.55→1.91；30 s 时间窗 + 模式阈值）、列表 `MediaStreams`、NFO 解码缓存（−0.28…0.36 ms/重复详情）、People/DTO 成本已量；ADR-049 |
| W6 双后端与边界 | 部分 | 迁移配对 + 后端分支两清单与校验器（已入 `AGENTS.md`）、存储接口两片拆分（邮件 outbox、邀请/注册邮件流程；`repository.rs` 9,222→5,948）、typed 化首个查询（`user_item_state`）；ADR-051 |

## 4. 下一步（按序，均可独立提交）

1. ~~**W5：前台延迟反馈 → 后台并发预算**~~ 已完成（`3d7daa28`，默认关闭）并验证：媒体车道确有 CPU
   工作时比值 2.73/2.44/2.49（off）→ 1.77/2.07/1.88（on），均值 −25%，空闲 p95 不变、扫描期 p95 −20%；
   媒体车道空闲（重扫或本机无 ffprobe/ffmpeg）时无差异。默认阈值/开关策略仍待所有者定，细节见
   `LUX-DEVELOPMENT.md` W5「剩余」第 4 条。
2. ~~W5：`Fields` 投影~~ 列表/`Items?Ids=` 的 `MediaStreams` 已补（`dbd228e0`）；`People` 真批量经测量
   （0.1–0.5 ms/项）**不做**，先决条件见 W5 段。
3. ~~W5：NFO 解码 LRU~~ 已完成（`98819ae9`）：130 KB JSON 读取在重复详情上被跳过，实测 −0.28…−0.36 ms；
   剩余成本在 133 KB 响应体本身。
4. ~~W5：DTO `Value` 树~~ 已量（`4da06159`）：31 个条目值深克隆 0.118 ms/页 → **不重写**。
5. **W6：存储接口继续按域拆**（`0025f08c` 邮件 outbox、`54cfb581` 邀请/注册邮件流程；下一个建议
   收藏/评分或媒体写入），每次一个域、纯移动、只跑该域测试。
6. **W6：热点查询 typed 化**——`sqlx::query!` 在双后端下不可行，只能 `query_as` + 手写行结构；逐条评估，
   先看 CPU/请求基线里 SQL 占比最高的查询（搜索匹配/总数/排序、列表可见性）。
7. ~~W4：PostgreSQL 侧搜索入队~~ 已完成（`8a7d4011`）：`migrations-postgres/0239` 语句级入队触发器 + PG 消费者；真机 PG 16（OrbStack 容器 `lux-goal-postgres`，`127.0.0.1:55432`）上 34/34 忽略用例通过。剩余只是 PG 侧写预算基准（无扫描负载测量）。

## 5. 待决事项（需要项目所有者）

1. 门禁豁免是否接受（见 §2）。
2. ~~`tower-http` 的 `compression-gzip` 依赖~~ **已批准（2026-10-29）**：引入纯 Rust 的
   `async-compression` / `compression-codecs` / `compression-core`，`http-body` 转直连依赖；
   收益见 W5 段。不同意可移除，ETag/304 不受影响。
3. ~~搜索总数语义~~ **已决定（2026-10-29）：不改契约**。保留精确总数与现有排序；新增 `count=false` /
   Emby `EnableTotalRecordCount=false` 的“跳过计数”快路径（实测仅 −1.1 ms，成本主要在匹配与排序）。
   “搜索 CPU −40%”记为受 API 契约限制未达成（p95 余量 4.7×）；相关度排序留作显式 `sort=relevance` 参数。
4. 前台降档开关的默认值：时间窗与模式阈值已就位，翻默认是一行；未翻的两个理由（旧路径实例会常开、
   活体恢复验证需安静机器）见 W5「剩余」第 3 条。

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
# 媒体车道降档的扫描期/空闲 p95 比值（每臂一次，off/on 交替；--fake-media-cpu-ms 让媒体车道真有活）
python3 scripts/probe-media-lane-ab.py --throttle off --work-dir /tmp/lux-rm-resume3 --fake-media-cpu-ms 150
python3 scripts/probe-media-lane-ab.py --throttle on  --work-dir /tmp/lux-rm-resume3 --fake-media-cpu-ms 150
```

66k 夹具位置：`/tmp/lux-rm-resume3/config/lux.db`（若已被清理，用 `scripts/run-api-performance.py`
重新生成；认证 cookie 为 `lux_session=lux-api-performance-token`，CSRF 为 `spec-csrf-token`）。
