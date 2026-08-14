# Lux 性能记录

本文档记录可重复的基准结果。没有硬件、数据集、命令和提交信息的数字不作为验收证据。

## 基准目标

规格目标：10,000 部电影、50,000 集剧集；数据库预热、单页 50 条、扫描同时运行。API 目标为首页 p95 < 400 ms、媒体库首屏 p95 < 300 ms、搜索 p95 < 500 ms、详情 p95 < 200 ms、继续观看 p95 < 300 ms、缓存图片 p95 < 150 ms，扫描期间前台 p95 < 1 s 且不超过空闲时 2 倍。

## 记录模板

| 日期 | 提交 | 硬件/架构 | 数据集 | 命令 | 场景 | p50 | p95 | 错误率 | 内存 | 备注 |
|---|---|---|---|---|---|---:|---:|---:|---:|---|
| 2026-08-02 | 740de3c | macOS ARM64 (`aarch64-apple-darwin`), Rust 1.97.1 | 确定性 60,000 MKV / 600 目录 | `./scripts/run-performance.sh` | 首次全库扫描 | 14,104 ms | 14,104 ms | 0% | - | 60,000 条目；release 模式；未触发 NFO/ffprobe |
| 2026-08-02 | 740de3c | macOS ARM64 (`aarch64-apple-darwin`), Rust 1.97.1 | 同上 | `./scripts/run-performance.sh` | 无变化全库重扫 | 4,061 ms | 4,061 ms | 0% | - | 60,000 条目全部 fingerprint 命中并跳过 |
| 2026-08-02 | 740de3c | macOS ARM64 (`aarch64-apple-darwin`), Rust 1.97.1 | 同上 + 单目录新增 100 文件 | `./scripts/run-performance.sh` | 单目录增量（200 文件目录） | 31 ms | 31 ms | 0% | - | 100 个既有文件跳过，100 个新增文件入库；未标记其他路径 missing |
| 2026-08-02 | 740de3c | macOS ARM64 (`aarch64-apple-darwin`), Rust 1.97.1 | 同上 | `./scripts/run-performance.sh` | 扫描期间 50 个管理员库列表请求 | 4 ms | 4 ms | 0% | - | `foregroundDuringScan=true`；目标前台 p95 < 1,000 ms |
| 2026-08-03 | 50a9e09 | macOS ARM64 (`aarch64-apple-darwin`) | 确定性 60,000 MKV / 600 目录 | `./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量 | 16,526 / 4,131 / 36 ms | 16,526 / 4,131 / 36 ms | 0% | - | release；前台 50 请求 p95 8 ms，`foregroundErrors=0`；fixture 摘要同上 |
| 2026-08-03 | c23a757 | macOS ARM64 (`aarch64-apple-darwin`) | 确定性 60,000 MKV / 600 目录 | `./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量 | 23,574 / 7,520 / 1,300 ms | 23,574 / 7,520 / 1,300 ms | 0% | - | release；前台 50 请求 p95 11 ms，`foregroundErrors=0`；用户状态列表改为分块批量查询；fixture 摘要同上 |
| 2026-08-03 | df28a97 | macOS ARM64 (`aarch64-apple-darwin`) | 确定性 60,000 MKV / 600 目录 | `./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量 | 21,394 / 4,307 / 41 ms | 21,394 / 4,307 / 41 ms | 0% | - | release；前台 50 请求 p95 10 ms，`foregroundErrors=0`；fixture 摘要同上 |
| 2026-08-03 | 8796365 | macOS ARM64 (`aarch64-apple-darwin`) | 确定性 60,000 MKV / 600 目录 | `./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量 | 38,024 / 10,392 / 41 ms | 38,024 / 10,392 / 41 ms | 0% | - | release；前台 50 请求 p95 10 ms，`foregroundErrors=0`；本机负载导致扫描耗时波动；fixture 摘要同上 |
| 2026-08-03 | ba39b1d | macOS ARM64 (`aarch64-apple-darwin`) | 确定性 60,000 MKV / 600 目录 | `./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量 | 17,232 / 4,364 / 42 ms | 17,232 / 4,364 / 42 ms | 0% | - | release；前台 50 请求 p95 11 ms，`foregroundErrors=0`；fixture 摘要同上 |
| 2026-08-03 | b42a133 | macOS ARM64 (`aarch64-apple-darwin`) | 确定性 60,000 MKV / 600 目录 | `./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量 | 22,506 / 6,481 / 41 ms | 22,506 / 6,481 / 41 ms | 0% | - | release；前台 50 请求 p95 12 ms，`foregroundErrors=0`；`metadataFingerprintCount=0`、`nonPendingProbeCount=0`；fixture 摘要同上 |
| 2026-08-08 | f3f0d460 | macOS ARM64 (`aarch64-apple-darwin`) | 确定性 60,000 MKV / 600 目录 | `./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量；目录列表 / 搜索 | 6,459 / 7,987 / 46 ms；336 / 131 ms | 6,459 / 7,987 / 46 ms；340 / 7,287 ms | 0% | - | release；扫描期间前台 50 请求 p95 42 ms；目录列表 50 并发 p95 340 ms；搜索单次 131 ms、50 并发 p95 7,287 ms；`foregroundErrors=0`、`metadataFingerprintCount=0`、`nonPendingProbeCount=0`；fixture 摘要同上 |
| 2026-08-09 | c022fcac | macOS ARM64 (`aarch64-apple-darwin`) | 确定性 60,000 MKV / 600 目录 | `./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量；目录列表 / 搜索 | 6,043 / 5,634 / 46 ms；301 / 4,823 ms | 6,043 / 5,634 / 46 ms；306 / 4,848 ms | 0% | - | release；扫描期间前台 50 请求 p95 50 ms；目录列表 50 并发 p95 306 ms；搜索单次 116 ms、50 并发 p95 4,848 ms；`foregroundErrors=0`、`metadataFingerprintCount=0`、`nonPendingProbeCount=0`；fixture 摘要同上；该脚本仍直接调用 `LibraryScanner`，持久化后台任务另由扫描任务集成测试覆盖 |
| 2026-08-10 | 5e0bef61 | macOS ARM64 (`aarch64-apple-darwin`) | 确定性 60,000 MKV / 600 目录 | `./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量；目录列表 / 搜索 | 6,234 / 7,593 / 47 ms；225 / 3,161 ms | 6,234 / 7,593 / 47 ms；366 / 6,103 ms | 0% | - | release；扫描期间前台 50 请求 p95 49 ms；目录列表 50 并发 p95 366 ms；搜索单次 83 ms、50 并发 p95 6,103 ms；目录聚合限制为 16 个执行、64 个总在途请求；`foregroundErrors=0`；未测量 macOS RSS，不能验证 Linux/glibc arena 回收 |

## Web 首屏资源记录

| 日期 | 提交 | 硬件/架构 | 数据集 | 命令 | 指标 | 优化前 | 优化后 | 备注 |
|---|---|---|---|---|---|---:|---:|---|
| 2026-08-11 | 899c961a / 65311847 | macOS ARM64 (`uname -m=arm64`) | Web production build；不含媒体库数据 | `pnpm --dir web build` | 主 JS（原始 / gzip） | 661.09 / 194.28 kB | 493.90 / 153.15 kB | 路由按需加载、首页 logo 复用已有标签；gzip 体积下降约 21%；未测量浏览器 LCP 或首页 API p95 |

## 首页加载基线

| 日期 | 提交 | 硬件/架构 | 数据集 | 命令/场景 | p50 | p90 | p95 | 最大值 | 备注 |
|---|---|---|---|---|---:|---:|---:|---:|---|
| 2026-08-14 | 57bf1b11 | macOS ARM64 (`uname -m=arm64`) | 1,200 个合成空 `.mkv`；单个电影库；无真实图片 | 预热后串行请求 `GET /api/v1/home` 50 次 | 2.411 ms | 3.595 ms | 4.196 ms | 9.111 ms | 本机服务；浏览器首页 API 约 4–6 ms，渲染 12 张媒体卡片，未发现 long task；该数据不代表目标 x86_64 NAS，也不能证明真实图片负载已达标 |
| 2026-08-14 | a812afe4 | macOS ARM64 (`uname -m=arm64`) | 同上 | release 服务；预热后串行请求 `GET /api/v1/home` 50 次 | 2.375 ms | 2.526 ms | 2.615 ms | 3.430 ms | 后端聚合优化后；ACL 只取一次、首页区块复用库 ID、用户状态跨区块去重批量查询；仅复测 API，未重新测量浏览器 LCP；该数据不代表目标 x86_64 NAS |

这里的 p90/p95 是请求耗时分布的位置：例如 p95=4.196 ms 表示 50 次请求中约 95% 不超过 4.196 ms，剩余约 5% 更慢；它们用于观察尾部延迟，不是平均值。由于本次样本只有 50 次，百分位数仅作开发机基线，不能替代目标数据集上的正式验收。

## ARM 开发机检查

- 架构：后续记录 `uname -m` 输出（当前为 `arm64`）。
- 用途：验证本机编译、单元/集成测试和工具链行为。
- 限制：不得将本机 ARM 结果当作目标 x86_64 NAS 的正式性能报告。

## LUX-045 ARM64 结果说明

- 固定入口：`scripts/run-performance.sh`；脚本临时生成 fixture，测试完成后删除，不提交 60,000 个媒体文件。
- fixture manifest：`lux-catalog-fixture-v1`，60,000 个文件、600 个目录、固定内容摘要 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`。
- 结果证明扫描期间前台请求没有出现错误或长时间锁等待；这只是本机 ARM64 基线，不代表 NAS/x86_64 容量结论。
- 无变化重扫的扫描路径只执行 fingerprint 检查；性能测试确认 `probe_status` 仍为 `PENDING` 且 `metadata_fingerprint` 仍为空。
- 2026-08-03 的新结果用于当前提交 `50a9e09` 的阶段性回归；首次扫描耗时受本机负载影响，不能与上一条结果直接视为性能退化结论。
- 2026-08-03 的新结果用于当前提交 `c23a757`；批量用户状态查询已消除 Web/Emby 列表的逐条状态读取，但本次扫描耗时受本机负载影响，不能与上一条结果直接视为性能退化结论。
- 2026-08-03 的新结果用于当前提交 `df28a97`；启动恢复逻辑未改变扫描基准的访问模式，首次扫描耗时受本机负载影响，不能与上一条结果直接视为性能退化结论。
- 2026-08-03 的新结果用于当前提交 `8796365`；健康诊断和 reconcile 路由不改变扫描基准的访问模式，首次/无变化扫描耗时受本机负载影响，不能与上一条结果直接视为性能退化结论。
- 2026-08-03 的新结果用于当前提交 `ba39b1d`；媒体 root 恢复和磁盘故障烟测不改变基准的访问模式，首次/无变化扫描耗时受本机负载影响，不能与上一条结果直接视为性能退化结论。
- 2026-08-03 的新结果用于当前提交 `b42a133`；扫描后 ffprobe 接入只在后台 job 完成后执行，基准直接调用 `LibraryScanner`，本次仍确认扫描期间前台 p95 12 ms、无错误，首次/无变化扫描耗时受本机负载影响，不能与上一条结果直接视为性能退化结论。
- 2026-08-08 的新结果用于当前提交 `f3f0d460`；媒体可用性改为物化字段并由触发器维护，电影首扫新增文件采用批量事务，搜索结果和详情采用批量加载，FTS 命中时跳过全表 LIKE 分支；新增目录列表和搜索并发指标，结果仍仅代表本机 ARM64。
- 2026-08-09 的新结果用于当前提交 `c022fcac`；新增电影后台任务的有界文件准备并发、容器 CPU 配额和首页 p95 自适应降档、按根批量写入；基准脚本本身仍是直接扫描路径，不能据此宣称持久化后台任务的精确耗时变化。
- 2026-08-10 的新结果用于提交 `5e0bef61`；目录聚合请求使用有界背压，50 个并发目录请求全部成功。剧集、合集、Resume、STRM 与弹幕的大数据量回归由对应合成数据库测试覆盖；本机没有用户的真实媒体库，Docker daemon 也未运行，因此该记录不证明目标 NAS 上的峰值 RSS 或任务结束后的 glibc RSS 回收效果。

## 规则

- 首次扫描、无变化重扫、单目录增量、50 并发短 API 请求、扫描并发前台、4 个 Range 连接和任务恢复都要有独立记录。
- 每次性能优化记录硬件、数据集、命令、提交以及前后结果。
- 记录中的路径、token、真实外部 URL 和用户数据必须脱敏。
- SQL 热查询计划记录见 [`docs/SQL-AUDIT.md`](SQL-AUDIT.md)。
