# Lux 数据路径重构 · 交接说明（2026-10-28）

本文用于**在新会话里继续目标**。它只记录已经验证过的事实、当前状态、待决事项和下一步；权威规格仍是
`docs/LUX-DEVELOPMENT.md`（本文引用其段落编号不适用于其他文档）与计划工作稿
`docs/LUX-DATA-PATH-PLAN.md`（**该文件被 .gitignore 忽略，只存在于本机工作区**）。

## 0. 新会话开场提示（可直接粘贴）

> 继续 Lux 数据路径重构（W4 已完成、W5/W6 部分完成）。先读 `docs/LUX-DEVELOPMENT.md` 的
> 「数据路径重构（W0–W6）实施状态」表、W4/W5/W6 段与「门禁豁免清单」，以及
> `docs/LUX-DATA-PATH-HANDOFF.md`。工作区 `/Volumes/Toshiba/mywork/Lux`，分支 `test`，
> HEAD = `6a2922bd`（若已前进以 `git log` 为准）。W5/W6/W4 的剩余项已全部收口：所有者 2026-10-29 确认
> 降档默认 `off`、门禁口径（1 条书面豁免 `LUX-870` + 1 条环境观察）、P5 登记“部分达成”、W6 范围收口，
> 并决定暂缓 PG 写预算基准与全量门禁重跑；gzip 依赖与搜索总数语义同日决定（见 §5）。后续可选工作见 §4。
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

门禁现状（所有者 2026-10-29 确认）：**1 条书面豁免 + 1 条环境观察，不是全绿**。

- 书面豁免：`tests/reidentify.rs::fill_missing_dispatcher_serializes_jobs_for_one_library`
  （`Error: Elapsed(())`，既有问题、与本次改动无关：回退触发器仍 1/8 失败、等待放宽到 10 s 仍 4/12、
  同目标 25/25 通过、许可非饥饿）。后续任务 **`LUX-870`（未开始）**：查“另一个库的补全任务长时间不启动”。
- 环境观察（不占豁免名额）：2026-10-29 全量门禁 126/128 目标通过，`--lib` 与 `--test probe` 在**与 PG 测试
  并发**时失败；同线程数下单独复跑全绿（lib 1,379/0/32、probe 21/21）。**所有者决定暂不重跑全量门禁**，
  需要取证时按上面命令原样重跑并确认这两项仍全绿。

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
| W4 触发器入队与写预算 | 完成（两后端入队；PG 写预算基准由所有者决定暂不跑） | 扫描 0.42→0.37 s、WAL 11.5→11.0 MiB；触发器开销 89→13 ms；`scripts/run-scan-write-bench.py`；ADR-050 |
| W5 CPU/协议层 | 部分达成（所有者确认；降档默认 `off`、搜索 −40% 受契约限制） | 空闲退避、gzip（首页 92,674→8,308 B）、ETag/304、CPU/请求基线（首页 277.5 / 搜索 21.3 / 列表 2.1 ms）、前台 p95 → 媒体车道配额（默认关闭；比值 2.55→1.91；30 s 时间窗 + 模式阈值）、列表 `MediaStreams`、NFO 解码缓存（−0.28…0.36 ms/重复详情）、People/DTO 成本已量；ADR-049 |
| W6 双后端与边界 | 部分达成（所有者确认范围收口） | 迁移配对 + 后端分支两清单与校验器（已入 `AGENTS.md`）、存储接口两片拆分（邮件 outbox、邀请/注册邮件流程；`repository.rs` 9,222→5,948）、typed 化首个查询（`user_item_state`）；ADR-051 |

## 4. 下一步（W5/W6/W4 剩余项已收口；以下是后续可选工作）

1. **`LUX-870`（未开始）**：查“另一个库的补全任务长时间不启动”。线索：`METADATA_FILL_MISSING_*` 全局许可
   预占、dispatcher 1 s 合并窗口、被 gate 住的刮削器请求；候选修法 (a) 先 claim 再取许可、(b) 保留 1 个
   全局许可、(c) 两者组合。修好后从门禁豁免清单删除该条。
2. **读模型 `on` 是否成为默认**：这是删除旧路径的前置条件；所有者 2026-10-29 确认继续 `off` 默认。
   切换前要求：shadow 差异为 0 且稳定一个发布周期。
3. **旧路径清理**：在 `on` 默认化并稳定后，下一个发布周期删除 `off` 分支与旧查询（见 `LUX-DEVELOPMENT.md`
   的 P5 段）。
4. **延后的证据**（所有者 2026-10-29 决定暂不做）：PG 侧扫描写预算基准、空闲时的全量门禁重跑。
5. **可选优化**（未列入本轮范围）：搜索按相关度的 `sort=relevance` 加法参数；相对基线触发的前台降档；
   共享 45 列 catalog 行的 typed 化（需先统一 8 条 SELECT 形状）。

## 5. 已决事项（所有者 2026-10-29 确认，无需再问）

| 事项 | 决定 |
|---|---|
| `tower-http` gzip 依赖 | 批准（纯 Rust 传递依赖；兼容性条目已写） |
| 搜索总数语义 | 不改契约；新增 `count=false` / `EnableTotalRecordCount=false` 跳计数快路径（实测 −1.1 ms） |
| 前台降档默认值 | 保持 `off`（opt-in）；重评条件：改成相对基线触发，或拿到 NAS 上媒体车道竞争的数据 |
| 门禁豁免 | `reidentify` 接受豁免并登记 `LUX-870`；`lib`/`probe` 记为环境观察；全量门禁重跑暂缓 |
| P5 阶段门 | 登记“部分达成”（搜索 −40% 受契约限制；旧路径删除排在 `on` 默认化之后） |
| W6 范围 | 存储拆分到此为止；typed 化只做 4 个热点查询，45 列共享行暂缓 |
| LUX-869 三处偏差 | 接受现状（外部内容表 / trigram-tsvector / 搜索开关） |
| 读模型 `on` 默认 | 继续 `off`（切换前要求 shadow 差异为 0 并稳定一个发布周期） |
| PG 写预算基准、全量门禁重跑 | 暂缓（所有者决定先不跑） |

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
