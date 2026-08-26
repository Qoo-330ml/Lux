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
| 2026-08-15 | 8b2dca5a（工作树） | macOS ARM64 (`aarch64-apple-darwin`, `uname -m=arm64`) | 确定性 60,000 MKV / 600 目录 | `./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量；目录列表 / 搜索 | 183,854 / 8,538 / 55 ms；1,139 / 2,799 ms | 183,854 / 8,538 / 55 ms；1,840 / 4,605 ms | 0% | - | release；扫描期间前台 p95 93 ms；`foregroundErrors=0`、`metadataFingerprintCount=0`、`nonPendingProbeCount=0`；本次开发机负载下首扫明显慢于历史记录，不能据此归因于本改动或外推 NAS 性能 |

| 2026-08-21 | 7e0578a5 | macOS ARM64 (`aarch64-apple-darwin`, `uname -m=arm64`) | 确定性 60,000 MKV / 600 目录 | `./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量；目录列表 / 搜索 | 4,676 / 6,383 / 1,331 ms；35 / 3,120 ms | 4,676 / 6,383 / 1,331 ms；40 / 4,172 ms | 0% | - | release；电影身份与目录预取、filesystem/media_items/media_sources 批量写入；扫描期间前台 p95 154 ms，`foregroundErrors=0`；`metadataFingerprintCount=0`、`nonPendingProbeCount=0`；仅代表本机 ARM64，不外推 NAS/x86_64 性能 |
| 2026-08-21 | 60f3028c | macOS ARM64 (`aarch64-apple-darwin`, `uname -m=arm64`) | 同上 | `./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量；目录列表 / 搜索 | 4,687 / 6,406 / 1,393 ms；38 / 2,721 ms | 4,687 / 6,406 / 1,393 ms；44 / 4,219 ms | 0% | - | release；有界文件准备并发、目录 provider ID 批内复用、后台默认批次 100；扫描期间前台 p95 150 ms，`foregroundErrors=0`；`metadataFingerprintCount=0`、`nonPendingProbeCount=0`；与前一阶段同量级，说明优化保持稳定；仅代表本机 ARM64，不外推 NAS/x86_64 性能 |
| 2026-08-25 | 04b73f5d | macOS ARM64 (`aarch64-apple-darwin`, `uname -m=arm64`) | 确定性 60,000 MKV / 600 目录 | `LUX_PERF_FILE_COUNT=60000 ./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量；目录列表 / 搜索 | 4,524 / 6,854 / 1,599 ms；39 / 2,653 ms | 4,524 / 6,854 / 1,599 ms；44 / 4,396 ms | 0% | - | release；变化集后处理与默认 ffprobe 并发 64；扫描期间前台 p95 194 ms，`foregroundErrors=0`；`metadataFingerprintCount=0`、`nonPendingProbeCount=0`；搜索 p95 约 4.4 s，仍高于 500 ms 目标；仅代表本机 ARM64，不外推 NAS/x86_64 性能 |
| 2026-08-25 | 33fe9db4 | macOS ARM64 (`aarch64-apple-darwin`, `uname -m=arm64`) | 确定性 60,000 MKV / 600 目录 | `LUX_PERF_FILE_COUNT=60000 ./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量；目录列表 / 搜索 | 4,360 / 5,776 / 1,402 ms；40 / 2,643 ms | 4,360 / 5,776 / 1,402 ms；45 / 4,457 ms | 0% | - | release；fingerprint 命中时跳过逐文件索引修复查询；ffprobe 默认配置 128，扫描期间前台 p95 166 ms，`foregroundErrors=0`；`metadataFingerprintCount=0`、`nonPendingProbeCount=0`；搜索 p95 约 4.5 s，仍高于 500 ms 目标；仅代表本机 ARM64，不外推 NAS/x86_64 性能 |
| 2026-08-25 | 4b0561b2 | macOS ARM64 (`aarch64-apple-darwin`, `uname -m=arm64`) | 确定性 60,000 MKV / 600 目录 | `LUX_PERF_FILE_COUNT=60000 ./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量；目录列表 / 搜索 | 4,254 / 5,502 / 1,419 ms；47 / 2,653 ms | 4,254 / 5,502 / 1,419 ms；52 / 4,397 ms | 0% | - | release；已有文件 fingerprint/stat 使用最多 64 路有界 I/O 并发；ffprobe 默认配置 128，扫描期间前台 p95 170 ms，`foregroundErrors=0`；`metadataFingerprintCount=0`、`nonPendingProbeCount=0`；搜索 p95 约 4.4 s，仍高于 500 ms 目标；仅代表本机 ARM64，不外推 NAS/x86_64 性能 |

| 2026-08-25 | 2f4bf2cf | macOS ARM64 (`aarch64-apple-darwin`, `uname -m=arm64`) | 确定性 60,000 MKV / 600 目录 | `cargo test --release --locked --test performance lux_045_catalog_scan_benchmark -- --ignored --nocapture --test-threads=1`（fixture 由 `tools/catalog-fixture/generate.py` 生成） | 首次扫描 / 无变化重扫 / 单目录增量；目录列表 / 搜索 | 4,489 / 1,301 / 1,444 ms；40 / 217 ms | 4,489 / 1,301 / 1,444 ms；50 / 322 ms | 0% | - | release；同一用户、权限范围、查询和分页的在途搜索请求使用 singleflight；ffprobe 配额为默认 256、硬上限 512；扫描期间前台 p95 183 ms，`foregroundErrors=0`、`metadataFingerprintCount=0`、`nonPendingProbeCount=0`；搜索 p95 已低于 500 ms；仅代表本机 ARM64，不外推 NAS/x86_64 性能 |
| 2026-08-25 | 80aacea3 | macOS ARM64 (`aarch64-apple-darwin`, `uname -m=arm64`) | 同上 | 同上 | 首次扫描 / 无变化重扫 / 单目录增量；目录列表 / 搜索 | 4,730 / 1,420 / 1,537 ms；46 / 284 ms | 4,730 / 1,420 / 1,537 ms；53 / 394 ms | 0% | - | release；补充失败 search flight 唤醒修复；singleflight、ffprobe 256 默认/512 硬上限保持；扫描期间前台 p95 193 ms，`foregroundErrors=0`、`metadataFingerprintCount=0`、`nonPendingProbeCount=0`；搜索 p95 仍低于 500 ms；仅代表本机 ARM64，不外推 NAS/x86_64 性能 |
| 2026-08-25 | cf8a567a | macOS ARM64 (`aarch64-apple-darwin`, `uname -m=arm64`) | 确定性 60,000 MKV / 600 目录 | `LUX_PERF_FILE_COUNT=60000 ./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量；目录列表 / 搜索 | 4,651 / 1,368 / 1,519 ms；46 / 302 ms | 4,651 / 1,368 / 1,519 ms；54 / 412 ms | 0% | - | release；完整 LUX-045/LUX-197 脚本；singleflight 失败唤醒修复、ffprobe 256 默认/512 硬上限；扫描期间前台 p95 196 ms，`foregroundErrors=0`、`metadataFingerprintCount=0`、`nonPendingProbeCount=0`；搜索 p95 低于 500 ms；仅代表本机 ARM64，不外推 NAS/x86_64 性能 |

## LUX-197 ffprobe 并发记录

ffprobe 合成基准包含 512 个文件，`observed` 是 fake ffprobe 进程的最大重叠数。资源背压会根据本机 CPU、内存
和前台压力把实际值压低，因此 `requested` 是配置值，不是强制启动数。fake ffprobe 使用单进程 Python helper，
只用文件锁保护计数，不额外派生 sleep 子进程，避免测试工具自身放大高并发压力。

| 日期 | 提交 | 架构 | 请求并发 | 实测最大并发 | 耗时 | 命令 |
|---|---|---|---:|---:|---:|---|
| 2026-08-25 | cf8a567a | macOS ARM64 (`uname -m=arm64`) | 128 | 49 | 3,506 ms | `LUX_PERF_FILE_COUNT=60000 ./scripts/run-performance.sh` |
| 2026-08-25 | cf8a567a | macOS ARM64 (`uname -m=arm64`) | 256 | 91 | 3,192 ms | 同上 |
| 2026-08-25 | cf8a567a | macOS ARM64 (`uname -m=arm64`) | 384 | 72 | 3,115 ms | 同上 |
| 2026-08-25 | cf8a567a | macOS ARM64 (`uname -m=arm64`) | 512 | 75 | 3,120 ms | 同上 |
| 2026-08-25 | 345c6d3a | macOS ARM64 (`uname -m=arm64`) | 128 | 62 | 3,191 ms | `cargo test --release --locked --test performance lux_197_ffprobe_concurrency_benchmark -- --ignored --nocapture --test-threads=1` |
| 2026-08-25 | 345c6d3a | macOS ARM64 (`uname -m=arm64`) | 256 | 68 | 2,898 ms | 同上 |
| 2026-08-25 | 345c6d3a | macOS ARM64 (`uname -m=arm64`) | 384 | 63 | 2,917 ms | 同上 |
| 2026-08-25 | 345c6d3a | macOS ARM64 (`uname -m=arm64`) | 512 | 89 | 2,913 ms | 同上 |
| 2026-08-25 | 4b0561b2 | macOS ARM64 (`uname -m=arm64`) | 64 | 45 | 20,512 ms | `LUX_PERF_FILE_COUNT=60000 ./scripts/run-performance.sh` |
| 2026-08-25 | 4b0561b2 | macOS ARM64 (`uname -m=arm64`) | 128 | 69 | 35,961 ms | 同上 |
| 2026-08-25 | 4b0561b2 | macOS ARM64 (`uname -m=arm64`) | 192 | 82 | 41,628 ms | 同上 |
| 2026-08-25 | 4b0561b2 | macOS ARM64 (`uname -m=arm64`) | 256 | 89 | 41,898 ms | 同上 |

这组结果证明 512 路配置可被接受且全局 semaphore 没有超过硬上限；当前开发机观察值受动态背压和进程启动开销影响，
不能据此声称目标 NAS 的实际吞吐。ffprobe 默认配置为 256；4/8/16 核环境的正常有效目标分别为 128/256/512，压力升高时会降档。本次 512 个源在四档请求下均成功完成。

## Web 首屏资源记录

| 日期 | 提交 | 硬件/架构 | 数据集 | 命令 | 指标 | 优化前 | 优化后 | 备注 |
|---|---|---|---|---|---|---:|---:|---|
| 2026-08-11 | 899c961a / 65311847 | macOS ARM64 (`uname -m=arm64`) | Web production build；不含媒体库数据 | `pnpm --dir web build` | 主 JS（原始 / gzip） | 661.09 / 194.28 kB | 493.90 / 153.15 kB | 路由按需加载、首页 logo 复用已有标签；gzip 体积下降约 21%；未测量浏览器 LCP 或首页 API p95 |

## Web 客户端 HEVC fallback 性能

这些结果只表示本机客户端处理能力，不代表目标 x86_64 NAS 性能。`speedX` 定义为媒体时长除以 Worker 的
解码/编码处理耗时；小于 1 表示客户端转码本身慢于实时播放。

| 日期 | 提交 | 硬件/浏览器 | 样本 | 命令/场景 | 媒体时长 | Worker 处理 | speedX | 丢帧/同步 |
|---|---|---|---|---|---:|---:|---:|---|
| 2026-08-17 | `fa39190a` | macOS arm64 / HeadlessChrome 151 | 3840×2160 HEVC Main 8-bit + AAC、MP4 | Playwright `ClientHevcEngine.setSource` + 播放 2 秒 + seek | 8,000 ms | 21,558.7 ms | 0.371 | 50 帧/0 丢帧；播放漂移 30 ms，seek 漂移 36 ms |
| 2026-08-17 | `fa39190a` | macOS arm64 / HeadlessChrome 151 | 3840×2160 HEVC Main10 10-bit、MP4、无音频 | 同上 | 4,086 ms | 18,929.3 ms | 0.216 | 24 帧/0 丢帧；seek 通过 |

流式播放增量 `43a7b8e6` 复测如下；`setSource()` 在首个视频片段进入 MSE 后返回，完整输入读取、解码、编码和 `endOfStream` 在后台继续。`presentedFrameGaps` 由 `requestVideoFrameCallback` 的 `presentedFrames` 序列计算；HeadlessChrome 的 `getVideoPlaybackQuality().droppedVideoFrames` 累计值与实际 presented-frame 序列不一致，因此不作为本次丢帧结论。

| 日期 | 提交 | 硬件/浏览器 | 样本 | 命令/场景 | 媒体时长 | Worker 处理 | speedX | 丢帧/同步 |
|---|---|---|---|---|---:|---:|---:|---|
| 2026-08-17 | `43a7b8e6` | macOS arm64 / HeadlessChrome 151 | 3840×2160 HEVC Main 8-bit + AAC、MP4 | Playwright 流式 `setSource` + 首段播放 + 完整转码 + seek | 8,000 ms | 17,383.5 ms | 0.460 | 47 个 presented frame callback、0 个 frame gap；首段返回 4,537 ms，完整 17,665 ms；seek 87 ms，音画差约 44 ms |
| 2026-08-17 | `43a7b8e6` | macOS arm64 / HeadlessChrome 151 | 3840×2160 HEVC Main10 10-bit HDR10、MP4、无音频 | 同上 | 4,086 ms | 18,227.5 ms | 0.224 | 4 个 presented frame callback、0 个 frame gap；首段返回 9,606 ms，完整 18,577 ms；seek 79 ms |

4K 两条记录均未通过实时性能门；播放器已把该状态暴露给用户，并建议原生客户端或降低清晰度。样本 SHA-256
和完整兼容性结论见 `docs/COMPATIBILITY.md`。

## 首页加载基线

| 日期 | 提交 | 硬件/架构 | 数据集 | 命令/场景 | p50 | p90 | p95 | 最大值 | 备注 |
|---|---|---|---|---|---:|---:|---:|---:|---|
| 2026-08-14 | 57bf1b11 | macOS ARM64 (`uname -m=arm64`) | 1,200 个合成空 `.mkv`；单个电影库；无真实图片 | 预热后串行请求 `GET /api/v1/home` 50 次 | 2.411 ms | 3.595 ms | 4.196 ms | 9.111 ms | 本机服务；浏览器首页 API 约 4–6 ms，渲染 12 张媒体卡片，未发现 long task；该数据不代表目标 x86_64 NAS，也不能证明真实图片负载已达标 |
| 2026-08-14 | a812afe4 | macOS ARM64 (`uname -m=arm64`) | 同上 | release 服务；预热后串行请求 `GET /api/v1/home` 50 次 | 2.375 ms | 2.526 ms | 2.615 ms | 3.430 ms | 后端聚合优化后；ACL 只取一次、首页区块复用库 ID、用户状态跨区块去重批量查询；仅复测 API，未重新测量浏览器 LCP；该数据不代表目标 x86_64 NAS |
| 2026-08-14 | 633bfe4f | macOS ARM64 (`uname -m=arm64`) | 同上 | 干净提交的 release 服务；预热后串行请求 `GET /api/v1/home` 50 次 | 2.384 ms | 2.622 ms | 2.658 ms | 3.876 ms | 独立复核；与上一条结果同量级；不代表目标 x86_64 NAS |

浏览器复核（633bfe4f，空媒体库）：测试账户登录后首页正常渲染；页面隐藏状态模拟 20 秒期间 `/api/v1/home` 请求数没有增加，恢复可见后约 2.5 秒内增加 1 次刷新。测试账户没有头像，因此控制台只有预期的头像 404；未以该空媒体库结果宣称真实图片 LCP 达标。

## 元数据刮削请求计数验证

| 日期 | 提交 | 硬件/架构 | 数据集/命令 | 场景 | 优化前 | 优化后 | 备注 |
|---|---|---|---|---|---:|---:|---|
| 2026-08-19 | `e447f24` | macOS ARM64 (`uname -m=arm64`) | 两个 TMDb 搜索候选；`cargo test --locked --test metadata_selection automatic_candidate_search_expands_only_the_best_result` | 自动匹配候选展开 | 2 个候选都完整请求详情、图片、演职员等 | 1 个候选完整请求 + 1 个搜索摘要 | 集成测试确认第二候选没有详情请求；这是请求计数验证，不代表真实 TMDb/NAS 延迟 |
| 2026-08-19 | `7350f68` | macOS ARM64 (`uname -m=arm64`) | TMDb stub；`cargo test --locked --test tmdb tmdb_client_coalesces_and_reuses_cached_requests` | 同一搜索请求连续执行两次 | 2 次上游请求 | 1 次上游请求 | 证明进程缓存命中；缓存文件恢复和 singleflight 另有单元测试 |

这里的 p90/p95 是请求耗时分布的位置：例如 p95=4.196 ms 表示 50 次请求中约 95% 不超过 4.196 ms，剩余约 5% 更慢；它们用于观察尾部延迟，不是平均值。由于本次样本只有 50 次，百分位数仅作开发机基线，不能替代目标数据集上的正式验收。

### LUX-200 阶段指标与回归验证

LUX-200 的后台元数据指标通过管理员健康资源接口中的 `resources.metadata` 暴露。计数器只使用固定低基数标签：
`search`、`bundle`、`get`、`images`、`credits`、`external_ids`、`trailers`，以及
`queue_wait`、`item_total`、`image_download`、`image_write`、`cache_persist`、`nfo_write` 阶段；不会包含用户 ID、完整 URL、token 或原始错误文本。
`stageP95Ms` 使用有界的最近样本窗口。缓存和 singleflight 分别记录 `cache.hit.count` 与 `cache.miss.count`，刮削器重试记录对应 capability 的 `retry.*.count`，图片累计字节记录在 `image.bytes`。
缓存落盘另记录 `cache.persist.success.count`、`cache.persist.error.count` 和 `stageP95Ms.cache_persist`，用于区分缓存命中收益与落盘背压。

| 日期 | 提交 | 验证 | 结果 | 限制 |
|---|---|---|---|---|
| 2026-08-26 | 工作树（`uname -m=arm64`） | `cargo test --locked --test metadata_selection fill_missing_only_requests_the_missing_image_capability` | 只缺 poster 时仅命中 `/3/movie/1/images`；补齐 poster 后第二次 `FILL_MISSING` 上游请求数为 0 | 本地 TMDb stub，非真实 TMDb/NAS 延迟 |
| 2026-08-26 | 工作树（`uname -m=arm64`） | `cargo test --locked --test image_writer image_downloads_respect_the_global_concurrency_limit` | 6 个并发图片写入在测试 semaphore=2 时最大并发不超过 2 | 证明配额边界，不代表上游吞吐 |
| 2026-08-27 | `8ab96ce7`（`uname -m=arm64`） | `cargo test --locked --test reidentify fill_missing_skips_complete_movie_without_scraper_request` | 完整电影 `FILL_MISSING` 上游请求数为 0；删海报后补全会重新产生请求 | 完整夹具包含 NFO rich details、人物关系和多 provider ID；本地 TMDb stub |
| 2026-08-27 | `de7aad98`、`118260b7`（`uname -m=arm64`） | `cargo test --locked --lib application::images::tests::permanent_upstream_status_does_not_schedule_image_retry`；`cargo test --locked --test image_writer successful_image_retry_clears_the_backoff_state` | 403 不安排 `next_retry_at`；临时失败到期后的成功下载将状态置为 `AVAILABLE` 并清除退避 | 状态机回归验证，不代表真实上游延迟或吞吐 |
| 2026-08-27 | `1eb460d2`（`uname -m=arm64`） | `./scripts/run-metadata-performance.sh`（连续 5 次） | 每次 32/32 条目成功；吞吐 30.9–37.0 条/秒；每次 32 次 search、32 次 bundle；图片 28 条可用、4 条明确不可用、1 次临时重试；代表性一次 `elapsed=918ms`、`stageP95Ms={bundle:4,image_download:0,image_write:78,item_total:469,nfo_write:147,queue_wait:31,search:3}`、`imageBytes=1876` | SQLite 最终元数据选择事务使用 `BEGIN IMMEDIATE`；修复前并发基准偶发 `SQLITE_BUSY`/`SQLITE_BUSY_SNAPSHOT`；仅代表本机 ARM64，不外推 NAS/x86_64 |

本机架构需以 `uname -m` 记录；ARM64 测试结果不能外推到目标 NAS/x86_64。

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
