# Lux 性能记录

本文档记录可重复的基准结果。没有硬件、数据集、命令和提交信息的数字不作为验收证据。

## 基准目标

规格目标：10,000 部电影、50,000 集剧集；数据库预热、单页 50 条、扫描同时运行。API 目标为首页 p95 < 400 ms、媒体库首屏 p95 < 300 ms、搜索 p95 < 500 ms、详情 p95 < 200 ms、继续观看 p95 < 300 ms、缓存图片 p95 < 150 ms，扫描期间前台 p95 < 1 s 且不超过空闲时 2 倍。

## 记录模板

| 日期 | 提交 | 硬件/架构 | 数据集 | 命令 | 场景 | p50 | p95 | 错误率 | 内存 | 备注 |
|---|---|---|---|---|---|---:|---:|---:|---:|---|
| 2026-09-08 | 12b20f2d（基于 7f683012） | macOS ARM64 (`uname -m=arm64`) | SQLite；先执行 1–117 迁移并写入代表性扫描数据，再执行 118 | `cargo test --locked --test storage scan_index_compaction_preserves_existing_rows_during_upgrade` | 已有数据库升级与扫描索引压缩回归 | 1 passed | - | 0% | - | `reconciliation_scan_entries` 数据、`scan_job_targets` 数据和外键检查均保留；只记录迁移正确性，不外推 PostgreSQL WAL、数据库体积或 NAS/x86_64 性能 |
| 2026-09-08 | 6bd90d21 | macOS ARM64 (`uname -m=arm64`) | SQLite；1,025 条无个人数据扫描发现路径 | `cargo test --locked --lib storage::repository::repository_tests::reconciliation_entries_use_scan_safe_batches -- --exact` | 扫描发现中间表批量写入 | 11 → 6 条 DML | - | 0% | - | 扫描专用批次从 100 增至 200；每条语句最多 800 个绑定参数，低于 SQLite 历史 999 参数上限；约减少 45.5% 批量写入语句；不外推 PostgreSQL WAL 或 NAS/x86_64 性能 |
| 2026-08-29 | 0c621c60 | macOS ARM64 (`aarch64-apple-darwin`, `uname -m=arm64`) | 确定性 60,000 MKV / 600 目录 | `./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量；目录列表 / 搜索 | 5,123 / 1,291 / 1,421 ms；56 / 104 ms | 5,123 / 1,291 / 1,421 ms；61 / 302 ms | 0% | - | release；前台 50 请求 p95 203 ms，`foregroundErrors=0`、`metadataFingerprintCount=0`、`nonPendingProbeCount=0`；后台剧集/混合库批量索引、指纹有界并发、混合分类 NFO 缓存、超大目录发现分块由 `scanning_jobs` 集成测试覆盖；本机 ARM64，不外推 NAS/x86_64 |
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
| 2026-08-29 | 20e102f0 | macOS ARM64 (`aarch64-apple-darwin`, `uname -m=arm64`) | 确定性 60,000 MKV / 600 目录 | `./scripts/run-performance.sh` | 首次扫描 / 无变化重扫 / 单目录增量；目录列表 / 搜索 | 6,128 / 1,511 / 22 ms；79 / 221 ms | 6,128 / 1,511 / 22 ms；85 / 321 ms | 0% | - | release；电影目录按批次读取已有索引、64 路有界 fingerprint 检查、并发准备新文件并批量事务写入；剧集无变化 fingerprint 检查并发执行，provider ID 回写串行去重；扫描期间前台 p95 241 ms，`foregroundErrors=0`、`metadataFingerprintCount=0`、`nonPendingProbeCount=0`；仅代表本机 ARM64，不外推 NAS/x86_64 |

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
| 2026-08-31 | `ca737f79` / `170e41a5` | macOS ARM64 (`uname -m=arm64`) | Web production build；不含媒体库数据 | `pnpm --dir web build` | 主入口 / PlayerPage / router / HLS（原始 / gzip） | 99.54 / 26.22；78.78 / 25.14；0 / 0；594.13 / 185.60 kB | 99.72 / 26.25；79.07 / 25.26；38.71 / 14.02；594.13 / 185.60 kB | 时间线 React 更新限制为 100 ms；bootstrap/session 请求可取消；react-router 共享分包从空 chunk 修正为可复用 chunk；HLS 仍仅在服务端 HLS 路径动态加载；未测量真实浏览器 LCP，结果仅代表本机 ARM64 |

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

### 推荐计算专项记录

| 日期 | 提交/工作树 | 架构 | 数据集 | 场景 | 结果 | 备注 |
|---|---|---|---|---|---|---|
| 2026-08-31 | 优化前基线 | macOS ARM64（`uname -m=arm64`） | 约 60,000 条媒体、约 65,000 条用户状态 | 评分中位数、推荐主查询、冷缓存完整推荐 | 约 53 ms、91 ms、144 ms | 中位数排序只在冷缓存执行；主成本是播放用户去重和分组临时 B-tree；不能外推 NAS/x86_64 |
| 2026-08-31 | 工作树 | macOS ARM64（`uname -m=arm64`） | 同上 | 冷启动完整推荐 / 同批次后续推荐 | 201.8 ms / 0.75 ms | 冷启动包含一次 180 天播放去重、收藏聚合和评分中位数；后续请求读取每日推荐 ID 和物化统计；本机 ARM64 结果不能外推 NAS/x86_64 |

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
| 2026-08-27 | `1be3f59e`（`uname -m=arm64`） | `./scripts/run-metadata-performance.sh`（release，单次复测） | 32/32 条目成功；`elapsed=772ms`、吞吐 41.4 条/秒；32 次 search、32 次 bundle；图片 28 条可用、4 条明确不可用、1 次临时重试；`stageP95Ms={bundle:3,image_download:0,image_write:44,item_total:215,nfo_write:60,queue_wait:18,search:3}`；`imageBytes=1876` | 本次拆分下载/写入配额后未见基准退化；该 benchmark 使用 adapter stub，不触发持久化 provider cache，`cache_persist` 由独立指标测试覆盖；仅代表本机 ARM64，不外推 NAS/x86_64 |
| 2026-08-27 | `00b7a472`（`uname -m=arm64`） | `./scripts/run-metadata-performance.sh`（release，连续 5 次） | 32/32 条目均成功；耗时 849–895 ms，吞吐 35.7–37.7 条/秒；每次 32 次 search、32 次 bundle；图片每次 28 条可用、4 条明确不可用、1 次临时重试；`stageP95Ms` 代表性范围为 `bundle:3–4,image_download:0,image_write:27–33,item_total:119–131,nfo_write:32–37,queue_wait:6–11,search:3–4`；`imageBytes=1876` | SQLite 默认 4 路元数据 worker，进程级硬上限 16；本机 ARM64，不能外推 NAS/x86_64；adapter stub 不触发持久化 provider cache |
| 2026-08-27 | `1c1c52e9`（`uname -m=arm64`） | `./scripts/run-metadata-performance.sh`（release，单次最终复测） | 32/32 条目成功；`elapsed=875ms`、吞吐 36.6 条/秒；32 次 search、32 次 bundle；图片 28 条可用、4 条明确不可用、1 次临时重试；`stageP95Ms={bundle:4,image_download:0,image_write:30,item_total:114,nfo_write:34,queue_wait:7,search:4}`；`imageBytes=1876` | 最终并发/压力降档实现复测；结果与前一组连续 5 次基准同量级；adapter stub 不触发持久化 provider cache；仅代表本机 ARM64，不外推 NAS/x86_64 |
| 2026-08-29 | `7b76f3bd`（`uname -m=arm64`） | `./scripts/run-metadata-performance.sh`（release，连续 3 次） | `FILL_MISSING` 候选无 credits 时跳过重复 `people.json`/人物关系索引写回；32/32 条目成功；耗时 525–598ms，吞吐 53.4–60.8 条/秒；每次 32 次 search、32 次 bundle；图片 28 条可用、4 条明确不可用、1 次临时重试；代表性 `stageP95Ms={bundle:3,image_download:0,image_write:29,item_total:78,nfo_write:28,queue_wait:4,search:3}`；`imageBytes=1876` | 相比此前 849–895ms 基线，提升受本机 I/O/调度噪声影响；仅代表本机 ARM64，不外推 NAS/x86_64；adapter stub 不触发持久化 provider cache |
| 2026-08-27 | `00b7a472`（`uname -m=arm64`） | `cargo test --locked --test postgres_database -- --ignored --nocapture`（临时 `postgres:16-alpine`） | PostgreSQL 空库迁移、核心状态、元数据优先级/锁定字段/图片/人物关系、重扫布尔投影和 STRM 配置共 4/4 通过 | 临时本地容器，测试完成后已删除；不代表生产 NAS 连接池或远程磁盘延迟 |

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

## LUX-266 Manifest 发现写入边界

- 新建全量扫描时，scan job、Manifest、root 状态和根目录 frontier 在同一短事务中创建；扫描目录只从 Manifest frontier 取出，不再把目录待办写入 `reconciliation_scan_entries`。
- 每个有界发现 chunk 在同一事务中追加不可变 observation、插入子目录 frontier、更新 Manifest/root/job 计数，并只在成功枚举目录的最终 chunk 标记该目录完成。取消或失败保留已提交 observation/frontier；未完成的 root 标记为 `INCOMPLETE`，不可进入后续缺失判断。
- 每条 observation 使用 11 个绑定参数，按 80 条/语句（最多 880 binds）写入；子目录按 200 条（600 binds）写入；为保持本任务增量独立，已发现文件暂由旧文件索引工作队列承接，按 200 条（最多 800 binds）写入。LUX-267 完成 Manifest delta apply 后再移除这段过渡桥接。
- `cargo test --locked --test scanning_jobs` 覆盖 1,025 个文件的跨批次发现、observation 指纹/重观察版本、取消时保留已提交 frontier，以及 root 不可用后恢复。该测试证明正确性和 SQLite 批次边界，不是耗时/吞吐基准；此任务未运行 release benchmark，也不据此声称扫描速度提升或推断 PostgreSQL/NAS 性能。

## LUX-270 Manifest SQLite/PostgreSQL 阶段门

2026-09-24 在本机 ARM64（`uname -m=arm64`，Rust `aarch64`）使用相同的固定 fixture 运行 release 基准。fixture 为 60,000 个文件、600 个目录，SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`。基准二进制报告的基线提交为 `4802c939`，测量包含其后的 LUX-270 工作树改动；数据仅用于同一 ARM64 开发机对照。

| 后端 | Manifest 首扫 | batches / p50 / p95 | SQL / DML | 无变化重扫 | 50 并发管理请求 p95 / 目录列表 p95 | 锁 / WAL |
|---|---:|---|---:|---:|---:|---|
| SQLite | 15.796 s | 640 / 23 ms / 28 ms | 30,581 / 13,870 | 1.289 s（41 batches） | 241 ms / 337 ms | `busy_timeout=5000 ms`；154 次 `BEGIN IMMEDIATE` admission 样本：p50 21 µs、p95 10,058 µs、max 10,113 µs、0 次错误 |
| PostgreSQL 16（本地临时容器） | 142.229 s | 640 / 82 ms / 505 ms | 32,436 / 13,870 | 10.589 s（41 batches） | 272 ms / 641 ms | 写入 WAL 596,331,453 bytes；5,197 次锁等待采样，观察到的最大 waiter 数为 0 |

两组均处理 60,000 个新文件；PostgreSQL 数据库为该次测试专用空库。执行命令：

```bash
LUX_PERF_FILE_COUNT=60000 \
LUX_PERF_TEST_FILTER=lux_270_manifest_job_scan_benchmark \
scripts/run-performance.sh

LUX_PERF_BACKEND=postgres \
LUX_PERF_FILE_COUNT=60000 \
LUX_PERF_TEST_FILTER=lux_270_manifest_job_scan_benchmark \
POSTGRES_TEST_HOST=127.0.0.1 \
POSTGRES_TEST_PORT=55432 \
POSTGRES_TEST_DATABASE=your_disposable_empty_database \
POSTGRES_TEST_USER=your_test_user \
scripts/run-performance.sh
```

同日以相同 fixture 在提交 `4802c939` 重跑 SQLite LUX-045 直接扫描：2.105 s、3,254 SQL、2,404 DML；LUX-270 最初的 SQLite Manifest 实现为 202.468 s、506,457 SQL、310,870 DML。此次 Manifest 批量 CAS/Delta 更新与索引化最新观察分页后，相比最初 SQLite Manifest 版本首扫约快 12.8 倍，SQL 约减少 16.6 倍，DML 约减少 22.4 倍。Manifest 首扫仍比直接扫描基线慢；两条路径的持久化与安全语义不同，不能将它们当作同一工作量下的等价耗时。PostgreSQL 结果仅为本机临时容器单次观测，不能将 ARM64 数值外推至 NAS/x86_64。SQLite 锁 admission canary 会每 100 ms 尝试一次 `BEGIN IMMEDIATE` 并立即提交，采样本身可能轻微扰动扫描；PostgreSQL 锁采样通过 `pg_stat_activity` 读取，SQL 计数中排除了这些监控查询。

### Manifest 首扫优化复测

2026-09-24 在本机 ARM64（`uname -m=arm64`）对同一 60,000 文件 / 600 目录 fixture 连续运行三次 SQLite release 基准，关闭写锁采样器以减少测量扰动。基准二进制显示提交 `ac869321`，扫描代码为其上的工作树修改。

| 场景 | 三次结果 | 中位数 | SQL / DML | 备注 |
|---|---:|---:|---:|---|
| Manifest 首扫 | 7.682 / 7.707 / 7.705 s | **7.705 s** | 10,343 / 5,418 | 120 个 500-delta apply 批次；小目录发现合并为 76 个事务；apply 中位数：应用侧 2.569 s、事务 3.786 s |
| Manifest 无变化重扫 | 1.254 / 1.251 / 1.215 s | **1.251 s** | — | 41 个批次，无索引 apply |
| 扫描期间前台 50 请求 | p95 231 / 268 / 234 ms | **234 ms** | — | 目录列表 p95 中位数 419 ms |
| 旧直接扫描器 | 1.994 s | — | 3,254 / 2,404 | 同机、同 fixture 的一次基准；不是与完整 Manifest 任务相同的持久化/恢复工作量 |

本轮对比 Manifest 初版 15.796 s，首扫中位数减少约 51.2%；SQL/DML 从 30,621/13,870 降至 10,343/5,418。与旧直接扫描 1.994 s 相比仍慢约 **3.9 倍**，**未通过“首扫不能比原版慢”的目标**。已测优化包括 500 条有界 apply 事务（SQL 仍按 SQLite 参数安全上限分块）、复用持久化 observation、按扫描并发并行准备且只保留一次最终设备/inode/fingerprint 复核、合并小目录发现 checkpoint、合并差异页事务、独立 500-path target SQL 块，以及按层级批量插入新电影父目录。三次复测中 apply 阶段仍占约 6.5 s，是下一轮主要优化对象。

三次观测有约 1 秒波动，故记录中位数；这些数字只代表本机 ARM64 与 SQLite，不外推 NAS/x86_64 或 PostgreSQL。LUX-267 v2 将以发现事务作为正向索引 checkpoint，避免为每个 ADD/CHANGE 持久化并二次应用 delta；只有完整根路径上的 REMOVE 仍走持久化 delta 与二次确认。当前旧直接扫描 1.994 s 是同 fixture 的性能参照，不代表相同持久化合同；重构目标是在保留不可变 observation、CAS、删除确认、取消恢复和原子 checkpoint 的前提下尽量逼近该参照。新实现须用同一 ARM64/SQLite fixture 三次 release 中位数测量，并另行运行 PostgreSQL 阶段门。

### LUX-267 v2 正向索引与 Manifest 写入复测

2026-09-25 在同一 ARM64（`uname -m=arm64`，Rust 1.97.1）与 SQLite release 环境，对 60,000 个 MKV / 600 个目录 fixture 连续测三次；SHA-256 为 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`。基准显示代码提交 `ac869321`，包括当前未提交的 LUX-267 修改；关闭 SQLite 写锁采样器。

| 场景 | 三次结果 | 中位数 | SQL / DML | 备注 |
|---|---:|---:|---:|---|
| Manifest 首扫 | 3.252 / 2.836 / 2.853 s | **2.853 s** | 4,383 / 2,953 | 13 个外层批次、66 个正向索引事务；正向提交阶段 1.897 / 1.862 / 1.845 s |
| Manifest 无变化重扫 | 0.966 / 0.959 / 0.968 s | **0.966 s** | — | 13 个批次 |
| 扫描期间前台 50 请求 | p95 236 / 227 / 228 ms | **228 ms** | — | 目录列表 p95 中位数 346 ms |

当前代码较 2026-09-24 的前一组 Manifest 复测中位数 3.100 s 快约 8%；受单次数据波动影响，不将差值全部归因于代码优化。v2 避免为正向文件构造不会持久化的 delta 对象；0134 另删除与主键列序完全相同的 `idx_scan_manifest_entries_path`。SQL/DML 数量未因此变化，表明本机测量没有清晰分离出该索引带来的耗时收益。旧直接扫描 1.994 s 仍只是较早提交的参照；本轮试跑该旧入口超过 4 分钟仍未完成且未输出首扫结果，已中止，因此没有当前版本的同代码对照。本记录不宣称已达到“不慢于原版”的目标，也不外推 PostgreSQL 或 NAS 性能。

### LUX-267 discovery format 3 混合 presence 复测

2026-09-25 在相同 ARM64/SQLite release 环境与 60,000 MKV / 600 目录 fixture 上运行三次；fixture SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`，代码提交 `ac869321` 加工作树修改，关闭 SQLite 锁采样器。

| 场景 | 三次结果 | 中位数 | SQL / DML | 备注 |
|---|---:|---:|---:|---|
| SQLite format 3 首扫 | 3.069 / 2.618 / 2.633 s | **2.633 s** | 3,727 / 2,475 | 66 个 1,000-path 正向事务；format=3、seen-path=0、完整 FILE observation=0、generation 标记 60,000 条 |
| SQLite format 3 无变化重扫 | 0.936 / 0.956 / 0.960 s | **0.956 s** | — | 13 个批次；seen-path ledger 记录 60,000 条未变化路径，generation 未被重写 |
| SQLite 扫描期间前台 50 请求 | p95 234 / 230 / 229 ms | **230 ms** | — | 目录列表 p95 中位数 364 ms |
| PostgreSQL 16 首扫（本机 ARM64 Docker） | 13.180 / 12.617 / 14.148 s | **13.180 s** | 3,802 / 2,475 | 66 个 1,000-path 事务；WAL 297,865,996 / 311,672,541 / 314,576,597 bytes |
| PostgreSQL 16 无变化重扫 | 4.032 / 4.015 / 4.040 s | **4.032 s** | — | 13 个批次；锁等待采样关闭 |
| PostgreSQL 16 扫描期间前台 50 请求 | p95 258 / 266 / 259 ms | **259 ms** | — | 目录列表 p95 中位数 635 ms |

与前一组 format 2 首扫中位数 2.853 s 相比，SQLite format 3 快约 **7.7%**，SQL/DML 分别减少约 15.0%/16.2%；无变化重扫由 0.966 s 到 0.956 s。成功正向索引由当前 filesystem generation 标记，因此新库首扫不写 seen-path；未变化、unstable 或 CAS 未成功路径才写 ledger。2,000-path 事务试验首扫中位数 2.653 s，慢于最终保留的 1,000-path 结果，故仍使用 1,000。

PostgreSQL 三次使用不同临时空库，format 3 首扫中位数 13.180 s、无变化重扫 4.032 s；这验证了本机 PostgreSQL 16 Docker 上的真实 migration、写入和删除合同。PG 锁等待采样关闭。历史旧直接扫描 1.994 s 来自较早提交；当前 ARM64/SQLite format 3 的 2.633 s 仍比该参照慢约 32%，本机旧入口超过 4 分钟未完成，未获得当前代码的直接对照。该差异和本机 Docker PG 数字都不外推 NAS/x86_64 或生产挂载盘。

### LUX-267 v3 target checkpoint 与双后端复测

2026-09-25 在相同 ARM64 环境、Rust 1.97.1、60,000 MKV / 600 目录 fixture（SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`）上，对加入 0136 target checkpoint 的工作树进行 release 基准。SQLite 连测五轮并关闭锁采样；PostgreSQL 16.15/aarch64 Docker 连测三轮，每轮使用新建的空数据库并采集锁等待和 WAL。索引耗时到 `POSTPROCESSING`；target 物化单独计时，处理 120,000 个 SOURCE/ITEM target。

| 后端 | 索引完成：各轮 / 中位数 | target 物化：各轮 / 中位数 | 无变化重扫：各轮 / 中位数 | SQL / DML | 前台 50 请求 p95 中位数 | WAL / 锁 |
|---|---:|---:|---:|---:|---:|---|
| SQLite | 2.058 / 2.111 / 2.018 / 2.008 / 1.950 s；**2.018 s** | 556 / 531 / 530 / 526 / 546 ms；**531 ms** | 925 / 941 / 936 / 917 / 915 ms；**925 ms** | 673 / 209 | 234 ms | `synchronous=FULL`；锁采样关闭 |
| PostgreSQL 16 | 8.703 / 9.657 / 10.591 s；**9.657 s** | 2.442 / 2.503 / 2.344 s；**2.442 s** | 3.641 / 5.031 / 3.743 s；**3.743 s** | 692 / 209 | 257 ms | WAL 215,655,557 / 217,383,695 / 219,460,717 bytes；三轮最大 waiter 均为 0 |

两种后端均使用 13 个扫描批次；PostgreSQL 有 10 个正向提交批次，正向提交中位数 7.268 s，准备中位数 607 ms。SQLite 正向提交中位数 1.150 s。PostgreSQL 目录列表 p95 中位数为 605 ms。性能 harness 的 target 计数/ready 查询已改为后端对应的 bind 占位符；此前 PostgreSQL 首轮基准因此报语法错误，该轮不纳入性能样本。

复测命令：

```bash
LUX_PERF_DISABLE_LOCK_MONITOR=1 \
LUX_PERF_FILE_COUNT=60000 \
LUX_PERF_DIRECTORY_COUNT=600 \
LUX_PERF_TEST_FILTER=lux_270_manifest_job_scan_benchmark \
scripts/run-performance.sh

LUX_PERF_BACKEND=postgres \
LUX_PERF_FILE_COUNT=60000 \
LUX_PERF_DIRECTORY_COUNT=600 \
LUX_PERF_TEST_FILTER=lux_270_manifest_job_scan_benchmark \
POSTGRES_TEST_HOST=127.0.0.1 \
POSTGRES_TEST_PORT=55432 \
POSTGRES_TEST_DATABASE=lux_perf_run_1 \
POSTGRES_TEST_USER=lux \
scripts/run-performance.sh
```

历史直接扫描器的 1.994 s 是较早提交的单次参照；本轮 SQLite Manifest 中位数高 24 ms（约 1.2%），且本轮样本范围为 1.950–2.111 s。两条路径的恢复和持久化工作不同，当前数据支持“约 2 秒索引完成”的结果，不能证明完整 Manifest 严格快于旧直接扫描。PostgreSQL 数字只代表本机 ARM64 容器，不推断远程数据库、NAS/x86_64 或生产挂载盘。

在 target-page / file-batch 参数整理后，用同一 SQLite fixture 又做三次 release spot check：索引完成 **2.913 / 2.005 / 2.042 s**，target 物化 **562 / 534 / 542 ms**，无变化重扫 **920 / 940 / 949 ms**，SQL/DML 为 **675 / 677 / 675 / 209**，target 数仍为 120,000。中位数分别为 2.042 s、542 ms、940 ms；首轮 2.913 s 是这一组三次的高值，因此保留完整样本供后续复测，不以它替换上面的五轮 SQLite 与三轮 PostgreSQL跨后端比较表。按这组三次 spot-check 中位数与历史 1.994 s 单次旧扫描参照相比，高 48 ms（约 2.4%）；仍不能把不同持久化/恢复语义的单次旧值视为严格同口径验收线。

### LUX-271 v3 资源感知扫描与目录 reader 实验

2026-09-25 在本机 ARM64（`uname -m=arm64`，Rust 1.97.1）使用同一 60,000 文件 / 600 目录 fixture，SHA-256 为 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`。基准构建提交为 `3f18aca1` 加工作树改动，关闭锁采样器。最终代码采用单 reader 顺序发现；基准报告的准备并发为 9，目录 reader 并发为 1。SQLite 与 PostgreSQL 均处理 13 个扫描批次、10 个正向提交批次和 120,000 个 postprocessing target。

| 后端 | 索引完成：各轮 / 中位数 | DISCOVERING / 正向准备 / 正向提交中位数 | target 物化中位数 | 无变化重扫中位数 | batch p50 / p95 中位数 | 前台 p95 / 目录列表 p95 中位数 | SQL / DML | WAL 中位数 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| SQLite，最终 sequential reader | 2.549 / 2.094 / 2.232 s；**2.232 s** | 2.224 / 0.592 / 1.247 s | 596 ms | 967 ms | 227 / 253 ms | 242 / 350 ms | 675 / 209 | — |
| PostgreSQL 16，最终 sequential reader | 55.841 / 9.078 / 16.646 s；**16.646 s** | 16.629 / 0.623 / 8.777 s | 2.875 s | 3.913 s | 928 / 3,944 ms | 270 / 595 ms | 692 / 209 | 215,871,368 bytes |

`DISCOVERING` 包含目录读取、frontier 和 checkpoint；正向准备与事务写入另有 tracing 计时。两后端的首扫样本波动明显。相较 LUX-270 基线，SQLite 索引中位数从 2.018 s 到 2.232 s、前台 p95 从 234 ms 到 242 ms、无变化重扫从 925 ms 到 967 ms；PostgreSQL 索引中位数从 9.657 s 到 16.646 s、前台 p95 从 257 ms 到 270 ms、无变化重扫从 3.743 s 到 3.913 s。该组数据没有通过 LUX-271 的性能门，也不能据此推断 NAS/x86_64 性能。

曾试过两路目录 reader。早期未限制 live reader 数的三轮中位数为 SQLite 2.114 s、PostgreSQL 9.606 s，但代码可能同时保留 64 个目录 reader，也未覆盖空目录替换后的身份复核，因此不能作为可接受结果。把 reader 数限制为两路并补完安全检查后，PostgreSQL 在新建独立数据库中的首扫观测为 10.511、15.855、55.030、14.632 s，波动过大，无法证明稳定收益。为遵守“没有可重复收益时不保留并发复杂度”的验收要求，最终代码移除了并行目录 reader；数据库写入全程仍为单写者。

LUX-271 的扫描配置优先级和目录替换安全检查已保留；并行目录 I/O 的性能验收未通过，项目尚不能据此关闭阶段 22。以上只代表本机 ARM64 与临时 PostgreSQL 16 容器。

## Web Bilibili 弹幕解析

基准脚本为 `scripts/run-danmaku-performance.mjs`，从指定 Git revision 加载优化前解析器，并与当前工作树在相同 Node 进程中交替执行。输入包含 5,000 条合法弹幕和一个超过 4 MiB 的 ASCII XML；每组 5 批、每批 30 个样本，报告各批 p50/p95 的中位数。

| 日期 | 提交 | 硬件/运行时 | 命令 | 场景 | 优化前 p50/p95 | 优化后 p50/p95 | 结果 |
|---|---|---|---|---|---:|---:|---|
| 2026-08-28 | `85db4549` | macOS ARM64 (`uname -m=arm64`), Node 24.14.1 | `LUX_DANMAKU_BASELINE_REF=85db4549^ node --expose-gc scripts/run-danmaku-performance.mjs` | 5,000 条、2,540,007 bytes 合法 XML | 13.423 / 13.735 ms | 13.197 / 13.565 ms | 解析结果均为 5,000 条 |
| 2026-08-28 | `85db4549` | macOS ARM64 (`uname -m=arm64`), Node 24.14.1 | 同上 | 4,194,311 bytes 超大 ASCII XML 大小检查 | 2.094 / 2.879 ms | 0.002 / 0.002 ms | 均返回 `INPUT_TOO_LARGE` |

该结果只代表本机 ARM64 Node 基准，不外推 NAS/x86_64 或所有浏览器；Chrome 151 本地浏览器实测同一合法夹具 p50/p95 为 8.1/8.6 ms。

## 规则

- 首次扫描、无变化重扫、单目录增量、50 并发短 API 请求、扫描并发前台、4 个 Range 连接和任务恢复都要有独立记录。
- 每次性能优化记录硬件、数据集、命令、提交以及前后结果。
- 记录中的路径、token、真实外部 URL 和用户数据必须脱敏。
- SQL 热查询计划记录见 [`docs/SQL-AUDIT.md`](SQL-AUDIT.md)。
### LUX-272 v3 全量扫描分阶段计时

LUX-272 在 `lux_270_manifest_job_scan_benchmark` 的完整 `ScanJobService` 路径中采集固定阶段名、微秒累计耗时、调用数、p50/p95 和处理单元数。报告的 `manifestIndexMs`、`postprocessingTargetMaterializationMs`、`unchangedRescanMs` 与前台请求 p95 是墙钟指标；`manifestStageTimings`、`targetStageTimings` 和 `unchangedRescanStageTimings` 是分项累计时间。分项可能嵌套或并发重叠，不能相加当作墙钟时间。

发现阶段分为 `directory_open`、`directory_readdir`、`directory_stat`、`directory_batch_total`、`baseline_query`、`positive_classification`、`positive_file_prepare` 和 `positive_file_recheck`。事务阶段分为输入校验、writer admission、Manifest 状态读取、目录 frontier 插入/完成、known-path 查询、root checkpoint、observation 插入、正向索引、presence ledger、Manifest/job 计数 checkpoint、commit 和事务总时间。`activePreparationTasksPeak` 与 `activeDirectoryReadersPeak` 取实测活动任务峰值；预算配置只作为背景值，不代替观测值。

事件只包含固定阶段名、微秒、计数和批次规模，不包含媒体名、路径、用户数据或数据库连接信息。每次 60k 报告必须包含首扫阶段 JSON、target 物化阶段、无变化重扫阶段、扫描墙钟时间、前台 p95、SQL/DML、SQLite 锁等待或 PostgreSQL WAL/锁等待。此阶段只增加诊断，不改变扫描调度或数据库语义。

验证命令（SQLite 与 PostgreSQL 各三轮 60k；每轮 PostgreSQL 使用新建空库）：

```bash
LUX_PERF_DISABLE_LOCK_MONITOR=1 \
LUX_PERF_FILE_COUNT=60000 \
LUX_PERF_TEST_FILTER=lux_270_manifest_job_scan_benchmark \
scripts/run-performance.sh

LUX_PERF_BACKEND=postgres \
LUX_PERF_FILE_COUNT=60000 \
LUX_PERF_TEST_FILTER=lux_270_manifest_job_scan_benchmark \
POSTGRES_TEST_HOST=127.0.0.1 \
POSTGRES_TEST_PORT=55432 \
POSTGRES_TEST_DATABASE=your_disposable_empty_database \
POSTGRES_TEST_USER=your_test_user \
scripts/run-performance.sh
```

2026-09-26 使用 Apple M4 / 16 GiB（`uname -m=arm64`，Rust 1.97.1），release 构建提交 `32b28d6a`。固定 fixture 为 60,000 个文件 / 600 个目录，SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`。SQLite 关闭 lock canary；PostgreSQL 16 使用本机一次性容器并启用 `pg_stat_activity` lock sampler。数据只代表本机 ARM64，不外推 NAS/x86_64。

| 后端 | 首扫索引完成：三轮 / 中位数 | 扫描批次；batch p50 / p95 中位数 | 正向准备 / 正向提交墙钟累计中位数 | target 物化 / 无变化重扫中位数 | 前台 p95 / 目录列表 p95 中位数 | SQL / DML 中位数 | WAL / 锁采样 |
|---|---:|---:|---:|---:|---:|---:|---|
| SQLite | 2.058 / 2.009 / 2.178 s；**2.058 s** | 13；208 / 264 ms | 523 / 1,208 ms | 546 / 961 ms | 259 / 383 ms | 675 / 209 | canary 关闭 |
| PostgreSQL 16 | 19.059 / 10.472 / 10.368 s；**10.472 s** | 13；835 / 2,036 ms | 411 / 7,365 ms | 2,523 / 5,145 ms | 281 / 685 ms | 694 / 209 | WAL 216,506,975 bytes；385 个锁等待采样，中位轮观察最大 waiter 数为 0 |

下表是三轮每轮阶段累计微秒的中位数。`directory_batch_total` 包括 reader 批次开销，readdir/stat 是其内部拆分；同理事务总时间包含内部 SQL 阶段，不能把这些行相加成墙钟时间。

| 阶段（累计微秒） | SQLite | PostgreSQL 16 |
|---|---:|---:|
| directory open | 39,790 | 66,485 |
| readdir | 11,919 | 19,425 |
| stat | 61,034 | 92,844 |
| directory batch total | 108,250 | 169,312 |
| baseline query | 17,397 | 2,128,641 |
| positive classification | 318,442 | 317,933 |
| positive file preparation | 770,302 | 788,440 |
| positive file recheck | 120,280 | 153,753 |
| positive index apply | 950,342 | 6,458,998 |
| presence ledger | 5,895 | 21,658 |
| transaction begin | 445 | 4,090 |
| transaction commit call | 218,952 | 75,649 |
| transaction total | 1,206,315 | 7,384,819 |

六轮的活动峰值均为 9 个文件准备任务、1 个目录 reader；这测量的是当前实际代码，暂未启用双目录读前。PostgreSQL `baseline_query` 三轮为 10.360 / 1.103 / 2.129 s，缓存和运行抖动明显；正向索引事务 `positive_index_apply` 中位数为 6.459 s，是当前 PG 主要成本之一。相较 LUX-270 的 SQLite 2.018 s / PostgreSQL 9.657 s 参考中位数，本次诊断版本为 2.058 s / 10.472 s，尚未通过阶段性能目标。

### LUX-273 双 reader 流水线 A/B 与回退决定

2026-09-26 在同一 Apple M4 / 16 GiB ARM64、Rust 1.97.1 和 60,000 文件 / 600 目录 fixture 上，对比 LUX-272 顺序 reader 和双 reader、有界 read/prepare/单 writer 流水线各三轮。两组使用相同 fixture SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`；PostgreSQL 使用本机一次性 PostgreSQL 16 容器。

| 后端/实现 | 首扫索引完成：三轮 / 中位数 | SQL / DML 中位数 | 正向提交批次 | target 物化 / 无变化重扫中位数 | 前台 p95 / 目录列表 p95 中位数 | WAL 中位数 |
|---|---:|---:|---:|---:|---:|---:|
| SQLite 顺序 reader（LUX-272） | 2.058 / 2.009 / 2.178 s；**2.058 s** | 675 / 209 | 10 | 546 / 961 ms | 259 / 383 ms | — |
| SQLite 流水线 | 1.951 / 2.136 / 1.935 s；**1.951 s** | 832 / 359 | 28 | 598 / 842 ms | 237 / 365 ms | — |
| PostgreSQL 顺序 reader（LUX-272） | 19.059 / 10.472 / 10.368 s；**10.472 s** | 694 / 209 | 10 | 2,523 / 5,145 ms | 281 / 685 ms | 216,506,975 bytes |
| PostgreSQL 流水线 | 10.987 / 17.588 / 14.756 s；**14.756 s** | 871 / 359 | 28 | 2,593 / 3,735 ms | 269 / 620 ms | 228,425,152 bytes |

六轮流水线基准均观察到两个活动 reader、两个并发目录读操作以及读/准备、读/提交重叠，在途峰值 7,454 / 8,192。它把 SQLite 首扫中位数缩短约 5.2%，但 SQL 增约 23%、DML 增约 72%；PostgreSQL 首扫中位数慢约 40.9%，SQL 增约 25%、DML 增约 72%，WAL 增约 5.5%。候选代码已按 LUX-273 条件移除：PostgreSQL 没有稳定收益，单后端加速不足以抵消另一后端回退。SQLite 与 PostgreSQL 数据仍只代表这台 ARM64 开发机和本机测试容器。

### LUX-274 正向索引写入子阶段诊断

2026-09-26 在同一 60,000 文件 / 600 目录 fixture 上各运行一轮，基于 `93a9fa3a` 的顺序 reader 路径，只增加固定名称的子阶段计时。单轮数据用于定位热点，不替代 LUX-272/LUX-273 的三轮中位数；PostgreSQL 此轮总耗时波动尤其明显。

| 后端 | 首扫索引 | `positive_index_apply` | add filesystem claim | add movie materialization | SQL / DML | target 物化 / 无变化重扫 | 前台 p95 | WAL |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| SQLite | 2.537 s | 0.978 s | 0.196 s | 0.755 s | 675 / 209 | 0.569 / 0.970 s | 234 ms | — |
| PostgreSQL 16 | 15.808 s | 6.212 s | 0.881 s | 5.260 s | 698 / 209 | 2.673 / 3.815 s | 278 ms | 216,191,375 bytes |

首扫 DML 摘要中，批量 `media_items` 插入 48 次、`media_sources` 插入 38 次、`filesystem_entries` 插入 38 次。PG 锁采样观察到 0 个最大等待者。电影项/来源物化占正向索引阶段的大部分耗时；LUX-274 试验了受参数上限约束的后端批次，SQL 数据合同和事务边界保持不变。单轮 PostgreSQL 数值不能与三轮中位数直接比较。

#### 后端有界批次三轮对比

每轮均使用 60,000 文件 / 600 目录的相同 fixture；LUX-274 顺序 reader 候选分三次使用新建空 PostgreSQL 数据库运行。SQLite 继续使用 2,000 行批次（最多 22,000 个 `media_sources` bind）；PostgreSQL 使用 5,000 行批次（最多 55,000 个 bind，低于 65,535 的参数上限）。SQLite 的 SQL 批次和 DML 数量保持不变。

| 后端/实现 | 首扫索引：三轮 / 中位数 | 正向索引 apply 中位数 | SQL / DML 中位数 | target / 无变化重扫中位数 | 前台 p95 中位数 | WAL 中位数 |
|---|---:|---:|---:|---:|---:|---:|
| SQLite LUX-272 基线 | 2.058 / 2.009 / 2.178 s；**2.058 s** | 0.950 s | 675 / 209 | 0.546 / 0.961 s | 259 ms | — |
| SQLite LUX-274 候选 | 2.539 / 2.111 / 2.146 s；**2.146 s** | 0.969 s | 675 / 209 | 0.570 / 0.986 s | 255 ms | — |
| PostgreSQL 16 LUX-273 顺序基线 | 19.059 / 10.472 / 10.368 s；**10.472 s** | 6.459 s | 694 / 209 | 2.523 / 5.145 s | 281 ms | 216,506,975 bytes |
| PostgreSQL 16 LUX-274 候选 | 9.692 / 9.354 / 8.758 s；**9.354 s** | 6.441 s | 616 / 152 | 2.617 / 3.847 s | 271 ms | 222,461,224 bytes |

PostgreSQL 候选将总 DML 减少约 27%、SQL 减少约 11%，三类主要批量写入分别从 48/38/38 次降为 29/19/19 次；索引完成中位数快约 10.7%，无变化重扫快约 25%。WAL 增加约 2.7%，锁等待采样最大 waiter 数仍为 0；正向索引阶段耗时基本持平，说明数据库行/索引写入仍是剩余成本。SQLite 仍走原 2,000 行批次，DML 不变；其首扫中位数比 LUX-272 参考高约 4.3%，target 和无变化重扫分别高约 4.4% 和 2.6%，前台 p95 改善约 1.5%。这组 ARM64 结果不证明 NAS 性能，也没有关闭 LUX-275 的严格双后端性能门。

### PostgreSQL provider 派生索引 statement trigger 复测

提交 `328d034b` 的迁移 `0141_statement_provider_index_refresh.sql` 将 `media_item_provider_ids` 的 INSERT/UPDATE 触发器改为 statement-level transition table。它避免大批量 `media_items` 写入时为每一行单独执行 provider 索引刷新；SQLite 没有对应迁移。迁移契约、空库启动、旧库升级和 provider 插入/更新语义测试均通过。

2026-09-26 在同一 Apple M4 ARM64、60,000 文件 / 600 目录 fixture、本机 PostgreSQL 16 容器上运行三轮。下面的数值用于定位剩余写入热点；由于尚未在同一环境对旧 row-trigger 版本完成三轮 A/B，不把它们表述为已证实的加速百分比。

| 指标 | 三轮 / 中位数 |
|---|---:|
| 首扫索引完成 | 6.266 / 6.172 / 5.957 s；**6.172 s** |
| `positive_index_apply` | 4.807 / 4.744 / 4.527 s；**4.744 s** |
| `movie_item_insert` | 2.186 / 2.213 / 2.098 s；**2.186 s** |
| `movie_source_insert` | 1.333 / 1.223 / 1.171 s；**1.223 s** |
| filesystem claim | 0.918 / 0.939 / 0.888 s；**0.918 s** |
| target 物化 | 2.412 / 2.380 / 2.502 s；**2.412 s** |
| 无变化重扫 | 2.285 / 2.037 / 2.102 s；**2.102 s** |
| 前台 p95 | 319 / 271 / 268 ms；**271 ms** |
| WAL | 约 219 MB |

同一扫描代码的 SQLite 对照（三轮，关闭 lock monitor）为首扫 2.683 / 2.964 / 2.893 s（中位数 2.893 s）、target 715 ms、无变化重扫 1.032 s、前台 p95 237 ms；provider 迁移未改变 SQLite 结果。以上数据只代表本机 ARM64，不外推 NAS/x86_64，且不关闭 LUX-275 阶段门。

迁移 `0142_filter_available_source_promotions.sql` 继续压缩 source INSERT 的 availability 路径：先筛选仍为 `has_available_source = 0` 的 item，再连接 `filesystem_entries`。同一 Apple M4 / PostgreSQL 16 / 60,000 文件 fixture 的三轮为首扫 6.596 / 6.384 / 6.578 s（中位数 6.578 s）、`movie_source_insert` 1.236 / 1.170 / 1.154 s（中位数 1.170 s）、`positive_index_apply` 5.131 / 4.741 / 4.965 s（中位数 4.965 s）、target 2.378 / 2.412 / 2.373 s（中位数 2.378 s）、无变化重扫 2.214 / 2.292 / 2.295 s（中位数 2.292 s）。与上一组三轮相比没有形成稳定的总耗时加速，因此保留它作为无语义变化的冗余探测削减，不把它计入 LUX-275 性能门收益。
