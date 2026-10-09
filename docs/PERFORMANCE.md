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

### 2026-09-30 首页请求路径优化

`2c0a02de` 将继续观看设置合并为一次读取；普通首页分页将继续观看条目与准确总数合并为一次 SQL（越界分页仍保留总数回退查询）。测试断言设置读取为 1 条 SQL、常规条目页和总数为 1 条 SQL。该版本的 `HomeService` 按用户、权限和可见媒体库范围缓存静态首页区块，并通过每个 cache entry 的计算锁合并同键快照重建；继续观看在每次 `/api/v1/home` 请求中重新查询。

### 2026-10-01 首页区块拆分

`cf734932` 更新 LUX-082：Lux Web 不再请求聚合 `/api/v1/home`。`/api/v1/home/carousel` 单独读取推荐轮播；HomeService 现在只缓存推荐列表，继续观看、媒体库、兼容首页响应都实时读取。继续观看由 `/api/v1/continue-watching` 提供，每库最新资源由 `/api/v1/libraries/{libraryId}/latest` 提供，后者沿用剧集按最新可用分集加入时间排序的查询。

首页每个区块有独立 TanStack Query 状态和 15 秒客户端超时，任一超时只显示该区块的重试状态。sessionStorage 只保存 `recommended`，home SSE 同时使服务端轮播缓存、浏览器轮播缓存及活动首页区块失效；最新资源 SSE 刷新覆盖刮削完成后新的图片标签。`/api/v1/home` 仍供已有 Lux 客户端兼容，继续遵守原 2 秒 worker 排队上限和 `Retry-After: 2`。

验证：`cargo test --locked --all-targets` 全部通过；需要 PostgreSQL 或人工性能基准的测试按套件标记为 ignored。`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets --all-features -- -D warnings` 通过；Web 75 个测试文件、536 项通过，`pnpm --dir web build` 成功。

这些是行为和构建验证，不是性能基准。测试环境为 `arm64`；目标平台首页 p95、扫描并发延迟和生产浏览器仍未重新测量，不能据此宣称 400 ms 或 NAS 性能目标已达成。

`3409ec60` 将首页放入独立的有界 worker 池，容量保持与原 Catalog 池相同，避免其他目录流量占满首页执行名额；两类队列等待仍最多 2 秒，超限返回 `CATALOG_BUSY` 和 `Retry-After: 2`。Web 首页请求传递可取消信号，最长等待 15 秒；首次加载超时后立即显示错误和手动重试，有缓存时继续呈现缓存内容。

本次没有使用 10,000 部电影 / 50,000 集测试库或目标 NAS 重新测量 API p95、扫描期间延迟或真实浏览器 LCP；400 ms 首页 p95 目标仍未通过实测验收。本机实现与测试环境架构为 `arm64`，不能外推 NAS/x86_64 性能。

### 2026-09-30 线上首页慢查询复测

在目标 PostgreSQL 数据库上对 `/api/v1/home` 每库最新资源 ID 选取查询运行有 4 秒 `statement_timeout` 的只读 `EXPLAIN ANALYZE`。同一快照、9 个启用媒体库下，原查询耗时 3.556 s、读 33,400 个共享缓冲页；最终 CTE 方案耗时 0.566 s、读 11,556 页，选取部分约快 6.3 倍。最终计划先一次聚合 episode 的 `series_id`/`parent_id` 最新 `added_at`，再物化每库候选集合，消除了对 1,481 个剧集逐个重复计算分集 MAX。该测量只涵盖每库最新 12 条资源 ID 选取，不含 DTO 图片/轨道加载、继续观看、响应传输，属于单次线上计划复测，不代表完整 API p95。

本轮浏览器复现记录到多次 `/api/v1/home` 在 15 秒客户端期限内没有响应字节后被取消，也有成功请求返回 HTTP 200、约 360 kB、耗时 4.65–6.31 s；健康页同时显示 PostgreSQL 数据库和活动元数据重识别工作。`EXPLAIN ANALYZE` 的基线计划在每个库对数百个不可用剧集分别执行分集 MAX/可见性子查询；最新分集时间 CTE 将这些重复索引访问合并为一次 episode-root 聚合。指标来自 2026-09-30 的这台服务器运行快照，不能外推成 NAS 的长期 p95。

`400 ms` 是完整首页 API p95 目标，不能由上述一条 SQL 的加速结果替代；后续仍须在 10,000 部电影 / 50,000 集数据量和扫描并发下复测完整端点与浏览器首屏。

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
`queue_wait`、`queue_claim`、`item_total`、`image_download_queue`、`image_download`、
`image_write_queue`、`image_write`、`cache_persist`、`nfo_write` 阶段；不会包含用户 ID、完整 URL、token 或原始错误文本。
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
| 2026-09-30 | `edb28dbb`（`uname -m=arm64`） | `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target ./scripts/run-metadata-performance.sh`（release，连续 3 次） | 32/32 条目均成功；耗时 599–662ms，中位数 608ms；吞吐 48.3–53.4 条/秒；每次 32 次 search/bundle，图片 28 条可用、4 条明确不可用、1 次临时重试；`stageP95Ms` 范围 `{bundle:3–4,image_download:0,image_download_queue:0,image_write:26–33,image_write_queue:0,item_total:92–129,nfo_write:30–37,queue_claim:5–8,queue_wait:5–8,search:3}`；`imageBytes=1876` | 仅本机 ARM64/SQLite、32 条固定夹具和 adapter stub；没有匹配改动前的同机 A/B，不据此声称吞吐提升；不代表 NAS/x86_64 或独立 PostgreSQL 的 CPU/延迟 |
| 2026-09-30 | `a420389f`（`uname -m=arm64`） | `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target ./scripts/run-metadata-performance.sh`（release，连续 3 次） | 32/32 条目均成功；耗时 568–633ms，中位数 584ms；吞吐 50.5–56.3 条/秒，中位数 54.8；每次 32 次 search/bundle，图片 28 条可用、4 条明确不可用、1 次临时重试、scraper 重试为 0；`stageP95Ms` 范围 `{bundle:3–4,image_download:0,image_download_queue:0,image_write:24–28,image_write_queue:0,item_total:86–213,nfo_write:27–36,queue_claim:5–7,queue_wait:5–7,search:3–4}`；`imageBytes=1876` | 仅本机 ARM64/SQLite、32 条固定夹具和 adapter stub；本基准不运行持久化 provider cache 或 `ScraperPluginClient`，不测真实插件、PostgreSQL、NAS/x86_64 或容器 CPU；没有严格同机 A/B，不据此声称 CPU 降幅或吞吐提升 |

本机架构需以 `uname -m` 记录；ARM64 测试结果不能外推到目标 NAS/x86_64。

## 本地 NFO page 运行指标

管理员资源快照中的 `metadata.counters` 固定记录本地 NFO page：

| 指标 | 口径 |
|---|---|
| `batch.local_nfo_page.count` | NFO page 调用次数 |
| `batch.local_nfo_page.items` | 每次调用入口处 source snapshot 条目数的累计值，包含后续被排除或校验为过期的条目 |
| `batch.local_nfo_page.max_items` | 单次调用入口处 source snapshot 的最大条目数 |
| `batch.local_nfo_page.success.count` / `batch.local_nfo_page.error.count` | 完整成功或含 item/批次错误的 page 数；一个 page 只计入其中一项 |
| `metadata.stageP95Ms.local_nfo_page` | 最近最多 128 个 page 耗时样本的 p95，毫秒 |

page 耗时从本地 NFO 批处理入口开始，覆盖 source 校验、NFO 读取与解析、人物处理和 deferred credits flush。批次/耗时样本是进程内资源指标；指标名采用固定白名单，不记录 item ID、媒体路径或错误文本。ScanJobService 的 outbox、job worker、手动 refresh 和 scan 后处理共享同一个 `ResourceMetrics` 实例。该记录描述观测口径，不是吞吐基准，也不代表 PostgreSQL、FNOS/NAS 或 CPU 收益。

### LUX-440 本地 NFO actor credits storage transaction 指标

本地 NFO 页 deferred actor credits 每次调用批量 replacement storage API 时记录一次固定名称指标。单个事务最多包含 16 个 item；若页内条目更多，则按实际 chunk 分别计数。

| 指标 | 口径 |
|---|---|
| `batch.local_nfo_actor_credits_tx.count` | 已尝试的 credits storage transaction 数；storage API 返回错误也计一次 |
| `batch.local_nfo_actor_credits_tx.items` | 送入 replacement API 的 item 数累计值 |
| `batch.local_nfo_actor_credits_tx.max_items` | 单个 transaction 的最大 item replacement 数 |
| `batch.local_nfo_actor_credits_tx.credit_entries` | 送入 replacement API 前，各 item relation 中 actor credit 条目数之和；不是去重后的数据库行数 |
| `batch.local_nfo_actor_credits_tx.success.count` / `batch.local_nfo_actor_credits_tx.error.count` | storage API 成功返回或错误返回的 transaction 数 |
| `metadata.stageP95Ms.local_nfo_actor_credits_tx` | 最近最多 128 个事务耗时样本的 p95，毫秒 |

事务计时包围完整的 storage replacement 调用，因此覆盖其内部锁等待、SQL 执行和 commit。credits transaction 失败会将对应 chunk item 标记为失败，也会反映在所属 NFO page 的错误计数中。指标只在进程内累计，不含 item/person ID、路径、标签或错误文本。测试验证输入数量、成功/失败、recent-window p95 和失败后的成功重试；这些自动化结果不代表 FNOS/NAS、PostgreSQL 延迟或 CPU 收益。

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

### PostgreSQL media_search/provider 刷新合并与未使用索引清理（0143）

迁移 `0143_merge_provider_refresh_into_media_search.sql` 将 provider lookup 刷新合并到已有的 `media_items` statement-level search trigger，并删除实际 `ILIKE '%term%'` 查询不使用的 `media_search(title)` 与 `media_search(sort_title)` B-tree。标题未变化时只在搜索行缺失的情况下补建；provider 字段未变化时不删除/重建 provider 行。SQLite 路径不变。

2026-09-26 在同一 Apple M4 ARM64、PostgreSQL 16 本机容器、60,000 文件 / 600 目录 fixture 上关闭锁采样，分别对干净 0142 worktree 和 0143 工作树各运行三轮。fixture SHA-256 为 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`。

| 版本 | 首扫索引完成：三轮 / 中位数 | `positive_index_apply` 中位数 | target 物化 | 无变化重扫 | 前台 p95 | SQL / DML | WAL 中位数 |
|---|---:|---:|---:|---:|---:|---:|---:|
| 0142 基线 | 15.242 / 15.266 / 15.015 s；**15.242 s** | 4.441 s | 2.394 s | 12.103 s | 267 ms | 421 / 122 | 219,210,636 bytes |
| 0143 工作树 | 14.771 / 13.171 / 14.642 s；**14.642 s** | 4.250 s | 2.378 s | 12.116 s | 272 ms | 419 / 122 | 209,838,782 bytes |

相对同机 0142 A/B，首扫中位数下降约 3.9%，`positive_index_apply` 下降约 4.3%，WAL 下降约 4.3%；无变化重扫增加约 0.1%，前台 p95 增加约 1.9%，均在 5% 回退门槛内。这个改动确认减少了 PostgreSQL 派生写入成本，但绝对首扫仍高于 LUX-270 的 9.657 秒参考，因此不关闭 LUX-275，也不能外推 NAS/x86_64。

### Manifest-lite 目录 frontier A/B（探索性）

2026-09-26 在同一 Apple M4 / 16 GiB ARM64、60,000 个文件 / 600 个目录 fixture 上，对比新扫描默认的 `workflow_version=2`、`discovery_format_version=3`、`discovery_mode=LITE` 与旧持久 frontier 路径。Lite 将目录 frontier 保存在进程内，子目录不写入 `scan_manifest_directories`；format 3 的文件存在性仍使用 `last_seen_generation` 和紧凑 seen-path ledger。SQLite 运行三轮，PostgreSQL 16 使用本机一次性容器运行一轮；旧持久 frontier 也各运行一轮，因此这组数据用于定位收益，不替代 LUX-275 的三轮同构 A/B。

| 后端/路径 | 首扫索引完成 | 正向提交批次；外层批次 | 无变化重扫 | 前台 p95 | WAL / 锁等待 |
|---|---:|---:|---:|---:|---:|
| SQLite Lite | 2.606 / 2.673 / 2.862 s；**2.673 s** | 8；11 | 约 1.03 s | 约 239–247 ms | — |
| SQLite 旧持久 frontier | 4.310 s | — | — | — | — |
| PostgreSQL 16 Lite | 6.051 s | — | 3.389 s | 274 ms | 225,100,660 bytes；0 |
| PostgreSQL 16 旧持久 frontier | 14.989 s | — | — | — | — |

Lite 的收益主要来自移除逐目录 frontier 的数据库写入和恢复查询；它不表示 Manifest 的全部语义可以删除。SQLite 当前中位数仍高于 LUX-270 的 2.018 秒参考，PostgreSQL 只有单轮结果，且所有数据只代表本机 ARM64 和临时数据库，不能关闭 LUX-275 或外推 NAS/x86_64。

实现与语义边界见 `docs/decisions/045-manifest-lite-discovery.md`。

### Jellyfin 风格目录批处理（已否决的历史对照）

2026-09-27 在同一台 Apple M4 / 16 GiB / ARM64 机器上，以相同的 60,000 文件、600 目录 fixture（SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`）交替运行三轮 Lite 与 Jellyfin 风格路径。SQLite 使用 `synchronous=FULL`，关闭会每 100 ms 写入一次的 SQLite 锁采样器；PostgreSQL 使用本机 Docker PostgreSQL 16.15 和每轮全新数据库，保留锁等待采样。两种路径都生成 120,000 个 targets，并在扫描期间采样 50 个前台请求。

Jellyfin 当前代码先完整收集一个目录的 child snapshot，再对同目录新增项成组 `CreateItems`，然后继续递归验证目录；`DirectoryService` 另有扫描期间的目录项、文件元数据和路径缓存。[Folder.cs](https://github.com/jellyfin/jellyfin/blob/390296c9c8160bb6ad6f01b41226398776d21a83/MediaBrowser.Controller/Entities/Folder.cs)、[LibraryManager.cs](https://github.com/jellyfin/jellyfin/blob/390296c9c8160bb6ad6f01b41226398776d21a83/Emby.Server.Implementations/Library/LibraryManager.cs)、[DirectoryService.cs](https://github.com/jellyfin/jellyfin/blob/390296c9c8160bb6ad6f01b41226398776d21a83/MediaBrowser.Controller/Providers/DirectoryService.cs)。Lux 曾实现一个仅供对照的原型：按父目录分别解析和提交，每目录约 100 个文件，因此同目录文件在一个有界批次内完成准备并单独提交；更大的目录仍按 Lux 的 chunk 上限流式处理。原型复用 Lux 的 Manifest、二次 stat、CAS、root 删除门槛和 target barrier，没有移植 Jellyfin 的 metadata/provider 对象模型，也没有引入其 scan-scoped cache。基于下方数据，逐目录提交显著增加数据库往返与事务固定开销；该原型和 feature 已从当前代码中移除，以下结果仅作为已否决方案的历史对照。

| 后端 / 方案 | 首扫索引完成（三轮；中位数） | 120k target 物化中位数 | 无变化重扫（三轮；中位数） | 正向提交批次 | SQL / DML | batch p95 | 前台请求 p95 | WAL 中位数 / 最大锁 waiter |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| SQLite Lite grouped | 2.880 / 2.335 / 2.300 s；**2.335 s** | 0.753 s | 1.027 / 1.044 / 0.968 s；**1.027 s** | 8 | 385 / 128 | 328 ms | 242 ms | — |
| SQLite Jellyfin folder batch | 2.744 / 2.688 / 2.708 s；**2.708 s** | 0.752 s | 9.737 / 9.842 / 9.877 s；**9.842 s** | 600 | 8,583 / 4,819 | 377 ms | 246 ms | — |
| PostgreSQL 16 Lite grouped | 6.026 / 6.124 / 5.881 s；**6.026 s** | 2.907 s | 3.380 / 3.091 / 3.138 s；**3.138 s** | 8 | 353 / 104 | 796 ms | 281 ms | 222,668,454 bytes / 0 |
| PostgreSQL 16 Jellyfin folder batch | 66.313 / 62.534 / 63.054 s；**63.054 s** | 2.308 s | 11.119 / 4.614 / 6.148 s；**6.148 s** | 600 | 9,215 / 4,819 | 15,831 ms | 270 ms | 227,071,287 bytes / 0 |

在这组均匀分布 fixture 上，目录模式让 SQLite 首扫慢约 16%，无变化重扫约 9.6 倍；PostgreSQL 首扫约 10.5 倍、重扫约 2 倍。PostgreSQL 的 batch p95 从 796 ms 增至 15.8 s。阶段计时也指向数据库路径：PostgreSQL `baseline_query` 累计中位数从 96 ms 增至 22.68 s，`positive_index_apply` 从 4.49 s 增至 31.25 s；目录 open/readdir/stat 累计合计约从 154 ms 增至 754 ms。阶段值是累计工作时间，不能相加当作墙钟。SQL/DML 增长与 8 → 600 个提交一致，而 WAL 仅增加约 2%，锁 waiter 仍为 0；主要代价是逐目录数据库往返和事务固定开销，不是锁争用。target 物化时间独立测量，不计入首扫索引完成时间。

历史方案供定位，不与上面的同构三轮 A/B 混算：

| 历史方案 | SQLite 首扫 | PostgreSQL 首扫 | 说明 |
|---|---:|---:|---|
| LUX-045 直接扫描（提交 `4802c939`） | 2.105 s | — | 旧版记录；不含本轮 Manifest / 120k target 完成口径 |
| 旧持久 frontier（2026-09-26 单轮） | 4.310 s | 14.989 s | 没有同轮重扫样本 |
| 最初 Manifest 实现 | 202.468 s | 142.229 s | LUX-270 初版，后来已大幅收敛 SQL/DML |

本轮又尝试运行当前工作树的 LUX-045 全流程基准；进程满核超过 5 分钟仍未结束，遂停止，未形成有效计时。因此历史 LUX-045 的 2.105 秒只能作为旧版本参考，不能宣称当前直接扫描快于 Manifest Lite。

结论：不采用逐目录提交方案，相关实现已移除。后续优化继续以 Lite 的有界跨目录读取和批量提交为基线；目录快照或 scan-scoped 文件系统缓存只有在独立测量证明收益后再考虑。该历史实验不关闭 LUX-275 阶段门，也不代表 NAS/x86_64 性能。

### NEW target 物化快速路径

2026-09-27 对 Lite grouped 的 target 物化增加 NEW 阶段快速路径：该阶段的输入已经限定为 `last_seen_change_kind = 'NEW'`，因此 ITEM target 直接写入 `NEW`，省去逐 item 查询同一 generation 是否存在 NEW source 的相关 `EXISTS`。CHANGED 阶段仍保留原判定，确保一个 item 同时含有 NEW 与 CHANGED source 时，ITEM target 仍优先标记 NEW。上方 Jellyfin A/B 的 Lite target 时间作为改动前同机基线。

同一 Apple M4 / 16 GiB / ARM64、本机 PostgreSQL 16.15、相同 60k/600 fixture 和 SQLite `synchronous=FULL` 配置下，候选代码分别运行三轮；每轮 PostgreSQL 使用新数据库，SQLite 关闭每 100 ms 写事务的锁采样器。结果只将 target 阶段与改动前基线比较：

| 后端 | 指标 | 改动前三轮中位数 | 快速路径三轮原始值 | 快速路径中位数 | 差异 |
|---|---|---:|---:|---:|---:|
| SQLite | 120k target 物化 | 0.753 s | 0.683 / 0.704 / 0.709 s | 0.704 s | 快约 6.5% |
| PostgreSQL 16 | 120k target 物化 | 2.907 s | 2.676 / 2.674 / 2.602 s | 2.674 s | 快约 8.0% |

快速路径的 SQLite target SQL/DML 为 94/34 条，PostgreSQL 为 103/34 条；PostgreSQL WAL 三轮中位数为 221,962,439 bytes，最大锁 waiter 为 0。候选运行的扫描索引中位数为 SQLite 2.898 s、PostgreSQL 6.216 s，无变化重扫为 0.993 s、3.179 s。索引计时在 target 代码运行之前结束，这段改动不会进入索引或重扫路径；这些跨时段差值不能归因于快速路径，也不能据此宣称首扫总时间改善。LUX-275 阶段门仍开放。

### 合并 target 写入与 16k 有界页

随后将每个游标页的 SOURCE 和 ITEM target 合并为同一条 `INSERT ... SELECT ... UNION ALL`，复用一次物化的 source page；CHANGED 页仍按 generation 检查 NEW 优先级。页大小从 8,000 调到 16,000 个 source，单次语句写入两类 target。下面比较同一候选路径的 8k 与 16k；硬件为 Apple M4 / 16 GiB / ARM64，fixture 为 60,000 files / 600 directories，SQLite 使用 `synchronous=FULL` 并关闭锁采样，PostgreSQL 16.15 每轮使用新临时库并采集 WAL 与锁等待。

| 后端 / 页大小 | 索引完成中位数 | 120k targets 中位数 | 无变化重扫中位数 | 前台 50 请求 p95 中位数 | target SQL / DML | target INSERT 次数 | batch p95 中位数 |
|---|---:|---:|---:|---:|---:|---:|---:|
| SQLite / 8k | 2.876 s | 684 ms | 961 ms | 237 ms | 86 / 26 | 8 | 未记录 |
| SQLite / 16k | 2.914 s | 629 ms | 962 ms | 234 ms | 50 / 14 | 4 | 401 ms |
| PostgreSQL 16 / 8k | 6.666 s | 2.660 s | 3.346 s | 283 ms | 95 / 26 | 8 | 933 ms |
| PostgreSQL 16 / 16k | 6.617 s | 2.466 s | 2.997 s | 263 ms | 55 / 14 | 4 | 937 ms |

16k 页使 target 阶段相对同一合并语句的 8k 页在 SQLite 快约 8.0%、PostgreSQL 快约 7.3%，并将 target INSERT 次数减半；索引、无变化重扫、前台 p95 与 batch p95 均未出现超过 5% 的回退。16k 的 PostgreSQL WAL 中位数为 207,871,938 bytes，三轮最大锁 waiter 为 0。SQLite 索引中位数 2.914 s 仍高于 LUX-270 的 2.018 s 参考，target 调整也不会改变索引计时范围；因此 LUX-275 严格门继续开放。此实验只代表本机 ARM64 与临时 PostgreSQL，不外推 NAS/x86_64。

### Lite 根目录状态更新去重

Lite 不把子目录 frontier 写入 `scan_manifest_directories`，但此前每个正向提交批次只要完成了任意目录，就会再次尝试更新该表中的根目录行；其 `state <> 'COMPLETE'` 条件让后续调用成为无效果 UPDATE。现在只在 Lite frontier 清空的收尾事务中更新根目录状态。对同一 60k/600 fixture 的 SQLite release 单轮测量，Lite 根目录状态 UPDATE 从 10 条降为 1 条，扫描 DML 从 128 条降为 119 条；首扫为 2.894 s、无变化重扫 969 ms、前台 p95 231 ms。首扫单轮与此前 2.914 s 三轮中位数基本相同，故仅记录冗余 SQL 的减少，不把它算作稳定加速。PostgreSQL 单轮为首扫 6.709 s、target 2.496 s、无变化重扫 3.212 s、前台 p95 259 ms、batch p95 932 ms，WAL 208,039,460 bytes、锁等待为 0；这组单轮只用于确认该路径可运行，不作为性能差异结论。LUX-275 仍开放。

当前基准入口只运行 Lite 路径。复跑时使用同一个 60k fixture；SQLite 设 `LUX_PERF_BACKEND=sqlite LUX_PERF_DISABLE_LOCK_MONITOR=1`，PostgreSQL 设 `LUX_PERF_BACKEND=postgres POSTGRES_TEST_DATABASE=<disposable-empty-db>`。release test 命令为：

```bash
CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target \
cargo test --release --locked \
  --test performance lux_270_manifest_job_scan_benchmark -- \
  --ignored --nocapture --test-threads=1
```

### LUX-275 两目录首批预读 A/B（已否决实验）

2026-09-27 对最近已提交的顺序目录 reader（`7952e4e5`）与候选实现做交错 release A/B。两版使用同一 SHA-256 为 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914` 的 60,000 文件 / 600 目录 fixture，每后端各三轮；每轮交替先跑的版本。硬件为 Apple M4 / 16 GiB（`uname -m=arm64`），PostgreSQL 为本机 PostgreSQL 16.15 容器、每次首扫使用独立空库；SQLite 使用 `synchronous=FULL` 并关闭 100 ms 锁采样器，PostgreSQL 保留锁等待采样。

候选只并行打开两个目录并预读各自第一页，处理仍按目录原顺序交给同一个 writer。reader 总数上限为 2，每路首批最多 4,000 个文件，数据库正向提交仍按 8,000 文件分批；没有引入读写流水线或并行事务。

| 后端 / reader | 首扫索引：三轮 / 中位数 | 120k target 中位数 | 无变化重扫中位数 | 前台 p95 / 目录列表 p95 中位数 | batch p95 中位数 | SQL / DML 中位数 | 正向提交批次 | WAL 中位数 / 最大 waiter |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| SQLite 顺序 | 2.763 / 2.177 / 2.246 s；**2.246 s** | 620 ms | 976 ms | 238 / 375 ms | 308 ms | 376 / 119 | 8 | — |
| SQLite 两目录首批预读 | 2.217 / 2.244 / 2.201 s；**2.217 s** | 628 ms | 933 ms | 236 / 383 ms | 312 ms | 376 / 119 | 8 | — |
| PostgreSQL 16 顺序 | 5.898 / 6.113 / 6.008 s；**6.008 s** | 2.513 s | 3.037 s | 264 / 309 ms | 796 ms | 344 / 95 | 8 | 210,321,664 bytes / 0 |
| PostgreSQL 16 两目录首批预读 | 5.778 / 5.791 / 5.805 s；**5.791 s** | 2.483 s | 2.955 s | 264 / 300 ms | 776 ms | 344 / 95 | 8 | 210,333,350 bytes / 0 |

预读实验的首扫中位数相对同机顺序版快约 1.3%（SQLite）和 3.6%（PostgreSQL）；target、无变化重扫、前台 p95 和 batch p95 中位数均未超过 5% 回退，DML、正向提交批次和 WAL 基本不变。SQLite 有一轮顺序版比预读版慢约 20%，另两轮差距约为 3% 内；因此 1.3% 的中位数变化没有越过本机运行波动。综合收益和增加的 reader 调度复杂度，不保留预读候选；代码继续使用顺序 reader。顺序版首扫中位数 2.246 秒仍比 LUX-270 的 2.018 秒参考慢约 11.3%，阶段 22 / LUX-275 性能门继续开放。这些本机 ARM64 结果不外推 NAS/x86_64。

### LUX-275 32k target page A/B

2026-09-27 对 16k 与 32k postprocessing target page 做交错 release A/B，各后端各三轮，使用与上节相同的 60k/600 fixture 和 Apple M4 / 16 GiB ARM64 环境。PostgreSQL 为每轮新建的本机 16.15 空库；SQLite 为 `synchronous=FULL` 且关闭锁采样器。32k 页仍有硬上限；查询用 seek cursor 和 LIMIT 取路径，SQL 每页只绑定固定数量的游标、generation 与 page limit，没有逐行 bind 参数。每页的 SOURCE 与 ITEM target 仍在单一 SQL、单一事务内一起提交，ready barrier 仍在所有根完成后推进。

| 后端 / page size | 首扫索引：三轮 / 中位数 | 120k target 物化：三轮 / 中位数 | 无变化重扫中位数 | 前台 p95 / 目录列表 p95 中位数 | batch p95 中位数 | target SQL / DML | target INSERT | WAL 中位数 / 最大 waiter |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| SQLite / 16k | 2.788 / 2.252 / 2.276 s；**2.276 s** | 617 / 630 / 635 ms；**630 ms** | 968 ms | 237 / 375 ms | 324 ms | 50 / 14 | 4 | — |
| SQLite / 32k | 2.173 / 2.185 / 2.155 s；**2.173 s** | 560 / 568 / 556 ms；**560 ms** | 976 ms | 234 / 381 ms | 299 ms | 32 / 8 | 2 | — |
| PostgreSQL 16 / 16k | 5.919 / 6.062 / 6.009 s；**6.009 s** | 2,400 / 2,566 / 2,549 ms；**2,549 ms** | 3,092 ms | 279 / 311 ms | 789 ms | 55 / 14 | 4 | 207,410,538 bytes / 0 |
| PostgreSQL 16 / 32k | 5.862 / 5.904 / 5.828 s；**5.862 s** | 2,629 / 2,356 / 2,523 ms；**2,523 ms** | 3,000 ms | 286 / 312 ms | 801 ms | 35 / 8 | 2 | 210,311,667 bytes / 0 |

32k 页把每后端的 target INSERT 从 4 条减到 2 条，target DML 从 14 条降至 8 条。target 阶段中位数 SQLite 快约 11.1%，PostgreSQL 快约 1.0%；PostgreSQL WAL 增约 1.4%，最大 waiter 仍为 0。无变化重扫、前台 p95、目录列表 p95 和 batch p95 中位数均未超过 5% 回退。首扫在 target 阶段之前已经计时，表中首扫差异是运行波动，不能归因于 page size。SQLite 仍未满足 LUX-270 的 2.018 秒索引完成参考，LUX-275 阶段门保持开放；这些本机数据不外推 NAS/x86_64。

### LUX-275 SQLite 搜索触发器空 alias 查询 A/B

2026-09-27 对提交 `71982fef` 的旧 INSERT trigger 与候选 migration `0146_skip_empty_alias_lookup_on_media_item_insert.sql` 做三轮交错 release 基准。旧 trigger 每插入一个媒体条目都会按 `item_id` 查询 `item_aliases` 并执行 `group_concat`；新媒体条目受外键保护，不可能在 INSERT trigger 前已有 alias，后续 alias 的插入/更新/删除仍由原有 alias trigger 刷新全文索引。新库启动时还会由 SQLite 兼容修复重建 `media_items`；因此 `src/storage/migration.rs` 中重建该 trigger 的定义也同步改为 `''`。

两版共用 SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914` 的 60,000 文件 / 600 目录 fixture，Apple M4 / 16 GiB / ARM64；SQLite 使用 `synchronous=FULL` 并关闭 100 ms 锁采样，PostgreSQL 使用本机 Docker PostgreSQL 16.15，每轮新建空数据库并保留锁等待采样。每次运行包含 120k target 物化、无变化重扫和扫描期间的 50 个前台请求。性能二进制直接执行 `lux_270_manifest_job_scan_benchmark --ignored --nocapture --test-threads=1`；原始 release test 总墙钟包括以上各阶段及基准初始化。

| 后端 / trigger | 首扫索引：三轮 / 中位数 | 120k target 中位数 | 无变化重扫中位数 | 前台 p95 中位数 | batch p95 中位数 | SQL / DML 中位数 | 基准总墙钟中位数 | WAL 中位数 / 最大 waiter |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| SQLite / 旧版 | 2.750 / 2.236 / 2.216 s；**2.236 s** | 581 ms | 978 ms | 240 ms | 322 ms | 376 / 119 | 4.25 s | — |
| SQLite / 空 alias 快速路径 | 2.085 / 2.157 / 2.069 s；**2.085 s** | 558 ms | 968 ms | 240 ms | 288 ms | 376 / 119 | 4.04 s | — |
| PostgreSQL 16 / 旧版 | 5.855 / 5.876 / 5.831 s；**5.855 s** | 2,327 ms | 3,061 ms | 275 ms | 780 ms | 344 / 95 | 13.49 s | 210,384,596 bytes / 0 |
| PostgreSQL 16 / SQLite-only migration | 5.887 / 5.822 / 5.984 s；**5.887 s** | 2,387 ms | 3,097 ms | 277 ms | 791 ms | 344 / 95 | 13.51 s | 222,086,857 bytes / 0 |

SQLite 首扫索引中位数快约 6.7%，基准总墙钟快约 4.9%；无变化重扫和前台 p95 基本持平，batch p95 下降约 10.6%，SQL/DML 数量不变。`positive_index_apply` 累计中位数为 1.076 → 1.062 秒；该值是批次累计工作时间，不是墙钟时间。首扫 2.085 秒仍比 LUX-270 的 2.018 秒参考慢约 3.3%，所以没有关闭 LUX-275。

PostgreSQL 代码路径未被这项 SQLite 优化修改；首扫中位数变化约 +0.5%，重扫、前台 p95 和 batch p95 均在 5% 以内，最大锁 waiter 为 0。WAL 中位数观察到 210.4 → 222.1 MB（约 +5.6%）；本轮 PG SQL/DML 计数相同，且每轮使用随机生成的条目 ID，因此该 WAL 差异的成因未由这次 A/B 确认，不归因于 SQLite migration。结果仅代表本机 ARM64 和临时数据库，不外推 NAS/x86_64。

复跑沿用本节前的 release 命令及同一 fixture；SQLite 设置 `LUX_PERF_BACKEND=sqlite LUX_PERF_SQLITE_SYNCHRONOUS=FULL LUX_PERF_DISABLE_LOCK_MONITOR=1`，PostgreSQL 设置 `LUX_PERF_BACKEND=postgres POSTGRES_TEST_DATABASE=<disposable-empty-db>`。候选版改动文件为 `migrations/0146_skip_empty_alias_lookup_on_media_item_insert.sql` 和 `src/storage/migration.rs`；alias 检索回归由 `tests/search.rs::fts_search_matches_chinese_titles_and_aliases_with_acl` 覆盖。

### SQLite provider-ID INSERT trigger 短路 A/B（未保留）

2026-09-27 在提交 `9cf43819` 上评估给 SQLite `media_item_provider_ids_ai` 增加 `WHEN NEW.provider_ids_json IS NOT NULL`，避免无 provider ID 的新条目执行一次 `json_each('{}')`。候选包含升级 migration 和启动时兼容重建修正；功能测试确认 providerless item 不生成派生索引行，实际 TMDB ID 仍进入索引。候选最后未保留，因为全链路没有改善。

基准继续使用同一 Apple M4 / 16 GiB / ARM64 和 60k/600 fixture（SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`），SQLite `synchronous=FULL`、锁采样关闭，每版三轮：

| SQLite trigger | 首扫索引：三轮 / 中位数 | 120k target 中位数 | 无变化重扫中位数 | 前台 p95 中位数 | batch p95 中位数 | SQL / DML | 基准总墙钟中位数 |
|---|---:|---:|---:|---:|---:|---:|---:|
| 原 trigger | 2.156 / 2.254 / 2.288 s；**2.254 s** | 580 ms | 1,010 ms | 247 ms | 320 ms | 376 / 119 | 4.29 s |
| providerless 短路候选 | 2.259 / 2.314 / 2.280 s；**2.280 s** | 583 ms | 983 ms | 253 ms | 326 ms | 376 / 119 | 4.35 s |

候选的 `movie_item_insert` 累计时间中位数从 459.0 降到 449.5 ms，但 `positive_index_apply` 基本持平（1,085.8 → 1,083.3 ms），首扫反而慢约 1.2%，基准总墙钟慢约 1.4%。因此 migration、兼容重建改动及其临时测试均已撤回；不将子阶段变化当作全链路收益。PostgreSQL 未重测，因为它不使用此 SQLite trigger 路径。

### SQLite/PostgreSQL 层级索引前缀去重 A/B（未保留）

2026-09-27 在 `67e1a0db` 基线上评估删除 `media_items(parent_id, removed_at)` 和 `(series_id, removed_at)` 两个窄索引；它们分别被 `(parent_id, removed_at, has_available_source)` 和 `(series_id, removed_at, has_available_source)` 的前缀覆盖。候选同步更新了 SQLite 兼容表重建逻辑，并由空库迁移删除两条旧索引。SQLite schema/EXPLAIN 回归和 PostgreSQL 空库启动迁移测试通过，确认层级查询仍命中保留的复合索引。

基线与候选在 Apple M4 / 16 GiB / ARM64、同一 SHA-256 为 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914` 的 60,000 文件 / 600 目录 fixture 上交错各跑三轮 release 基准。SQLite 为 `synchronous=FULL`、关闭锁采样；PostgreSQL 为本机 Docker 16.15，每轮使用新空库并采样锁等待。每轮还测 120k targets、无变化重扫和 50 并发前台请求。

| 后端/索引 | 首扫索引完成：三轮 / 中位数 | 120k target 中位数 | 无变化重扫中位数 | 前台 p95 中位数 | 目录列表 p95 中位数 | batch p95 中位数 | SQL / DML 中位数 | WAL 中位数 / 最大 waiter |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| SQLite / 基线 | 2.334 / 2.259 / 2.277 s；**2.277 s** | 578 ms | 1,007 ms | 230 ms | 366 ms | 324 ms | 376 / 119 | — |
| SQLite / 删除两条前缀索引 | 2.669 / 2.330 / 2.165 s；**2.330 s** | 564 ms | 984 ms | 236 ms | 375 ms | 319 ms | 376 / 119 | — |
| PostgreSQL 16 / 基线 | 5.894 / 5.968 / 6.041 s；**5.968 s** | 2,503 ms | 3,094 ms | 266 ms | 339 ms | 800 ms | 344 / 95 | 213,008,651 bytes / 0 |
| PostgreSQL 16 / 删除两条前缀索引 | 5.962 / 5.824 / 6.133 s；**5.962 s** | 2,406 ms | 3,090 ms | 264 ms | 356 ms | 812 ms | 344 / 95 | 198,472,015 bytes / 0 |

两后端首扫索引完成都没有稳定改善：SQLite 候选中位数慢约 2.3%，PostgreSQL 仅快约 0.1%。target 阶段略快，但不在首扫索引计时内；PG WAL 中位数低约 6.8%，现有三轮无法排除随机数据与写入波动，不能归因于索引删除。为遵守 LUX-275 的端到端收益门，候选 migration、兼容重建改动与测试均撤回；这一候选不保留为性能优化。数据只代表本机 ARM64，不外推 NAS/x86_64。

### PostgreSQL providerless INSERT trigger 快速路径 A/B

2026-09-27 在 `209b8754` 基线上评估 PostgreSQL migration `0146_skip_empty_provider_index_expansion.sql`。已有 statement-level `media_items` INSERT trigger 会把 transition table 每一行传给 `json_each_text`；扫描新媒体条目通常 `provider_ids_json IS NULL` 或 `{}`，因此候选先物化并过滤出非空 provider JSON，再调用 JSON table function。搜索索引仍为所有新条目写入，provider 派生索引只为非空 JSON 写入。SQLite 代码和 schema 未变。

基线和候选在 Apple M4 / 16 GiB / ARM64、60,000 文件 / 600 目录 fixture（SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`）上交错运行三轮 release 基准。PostgreSQL 为 Docker 16.15，每轮新空库并采样锁等待；SQLite 使用 `synchronous=FULL` 并关闭锁采样。每轮包括 120k target 物化、无变化重扫和 50 个前台请求。

| PostgreSQL 16 | 首扫索引完成：三轮 / 中位数 | `movie_item_insert` 累计中位数 | `positive_index_apply` 累计中位数 | 120k target 中位数 | 无变化重扫中位数 | 前台 p95 中位数 | 目录列表 p95 中位数 | batch p95 中位数 | SQL / DML 中位数 | WAL 中位数 / 最大 waiter |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 基线 | 6.143 / 6.184 / 5.993 s；**6.143 s** | 1,844 ms | 4,530 ms | 2,451 ms | 3,195 ms | 277 ms | 368 ms | 807 ms | 344 / 95 | 220,946,496 bytes / 0 |
| providerless JSON 快速路径 | 6.043 / 5.976 / 5.860 s；**5.976 s** | 1,793 ms | 4,433 ms | 2,453 ms | 3,165 ms | 268 ms | 374 ms | 800 ms | 344 / 95 | 221,833,274 bytes / 0 |

三组配对首扫都变快，候选中位数快约 2.7%；`movie_item_insert` 和 `positive_index_apply` 累计中位数分别下降约 2.8% 和 2.1%。target、无变化重扫、前台 p95 与 batch p95 中位数均未回退超过 5%，最大锁 waiter 为 0。WAL 仅高约 0.4%，不视为确定性变化。候选保留为 PostgreSQL 写入优化；它不改变 SQLite 指标，LUX-275 的 SQLite 首扫门仍开放。结果只代表本机 ARM64 和临时 PostgreSQL 容器，不外推 NAS/x86_64。

### SQLite sort-title FTS 重复 token 削减

2026-09-27 对 SQLite migration `0147_skip_redundant_sort_title_fts_tokens.sql` 做同 fixture 交错三轮 A/B。扫描器新建电影条目的 `sort_title` 通常只是 `title` 的 ASCII 小写形式；FTS5 对 ASCII 大小写不敏感，因此 insert/update trigger 在两者仅有 ASCII 大小写差异时将 FTS 的 `sort_title` 列置空，避免为每个 token 再写一份重复倒排项。判定使用 SQLite `NOCASE`，只跳过可证明安全的 ASCII 大小写差异；其他语言字符或真正不同的排序标题仍完整写入。`title`、`original_title`、aliases 与媒体库排序字段均未改变，已有 FTS 行不重建。

基线为 `5a3e1e5b`，在 Apple M4 / 16 GiB / ARM64 上，针对同一 SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914` 的 60,000 文件 / 600 目录 fixture 交错运行三轮。两版 release 测试二进制固定后复用同一 fixture；SQLite 使用 `synchronous=FULL`、关闭锁采样。每轮包括索引首扫、120k target 物化、无变化重扫和扫描期间 50 个前台请求。

| SQLite FTS trigger | 首扫索引：三轮 / 中位数 | `movie_item_insert` 累计中位数 | `positive_index_apply` 累计中位数 | 120k target 中位数 | 无变化重扫中位数 | 前台 p95 / 目录列表 p95 中位数 | batch p95 中位数 | SQL / DML 中位数 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 基线 | 2,307 / 2,295 / 2,289 ms；**2,295 ms** | 454.0 ms | 1,074.5 ms | 573 ms | 981 ms | 244 / 375 ms | 312 ms | 376 / 119 |
| 跳过重复 sort-title tokens | 2,163 / 2,247 / 2,285 ms；**2,247 ms** | 449.7 ms | 1,065.0 ms | 563 ms | 996 ms | 240 / 381 ms | 310 ms | 376 / 119 |

三组配对首扫均未回退，中位数快约 2.1%；`movie_item_insert` 累计中位数快约 0.9%。target、前台 p95 和 batch p95 均改善，无变化重扫慢约 1.5%，目录列表 p95 慢约 1.6%，均在 5% 回退门内。SQL/DML、批次数和准备并发不变。测试覆盖扫描生成的大小写等价 sort title 仍能通过 title 搜索，以及 update 后真正不同的 sort title 仍能被 FTS 搜索。优化仅影响 SQLite，PostgreSQL 路径未改；小幅收益只代表本机 ARM64，不外推 NAS/x86_64，LUX-275 双后端阶段门继续开放。

测量调用预先构建并固定基线/候选 release `performance` 测试二进制，之后按 `candidate, baseline` 顺序对同一 fixture 交错运行：

```bash
LUX_PERF_MEDIA_ROOT="$FIXTURE" \
LUX_PERF_FILE_COUNT=60000 \
LUX_PERF_BACKEND=sqlite \
LUX_PERF_SQLITE_SYNCHRONOUS=FULL \
LUX_PERF_DISABLE_LOCK_MONITOR=1 \
"$PERFORMANCE_TEST_BINARY" \
  --ignored --nocapture --test-threads=1 lux_270_manifest_job_scan_benchmark
```

### SQLite FTS5 移除未使用的 docsize 表 A/B

2026-09-27 评估 SQLite migration `0148_fts_columnsize_zero.sql`。SQLite FTS5 默认维护 `media_search_docsize`，保存每行每列的 token 数；Lux 搜索只使用 `MATCH`，没有 `bm25()`、`rank`、`snippet()` 或 `highlight()` 调用。候选保持默认 `detail=full` 和现有 tokenizer，只设置 [`columnsize=0`](https://sqlite.org/fts5.html#the_columnsize_option)，并重建索引以移除该 shadow table；迁移时从 `media_items` 和 `item_aliases` 重建现存索引，并继续跳过与标题仅 ASCII 大小写等价的 sort title。既有表字段、标点短语搜索、多个 token 的 AND 语义不变。没有选择 [`detail=column`](https://sqlite.org/fts5.html#the_detail_option)：它不支持短语查询，而 Lux 把一个空格片段整体加引号，标点会被 tokenizer 拆成多个词。

基线为 `ef403125`（含 0147），候选为其上的 0148 migration。在 Apple M4 / 16 GiB / ARM64、同 SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914` 的 60,000 文件 / 600 目录 fixture 上，对已固定的两版 release 测试二进制交错运行九轮；前六组候选先跑，后三组基线先跑。SQLite 使用 `synchronous=FULL`、关闭锁采样；每轮测索引首扫、120k target、无变化重扫与 50 个前台请求。

| SQLite FTS5 | 首扫索引：九轮 / 中位数 | `movie_item_insert` 累计中位数 | `positive_index_apply` 累计中位数 | 120k target 中位数 | 无变化重扫中位数 | 前台 p95 / 目录列表 p95 中位数 | batch p95 中位数 | SQL / DML 中位数 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 基线 | 2,271 / 2,279 / 2,242 / 2,199 / 2,200 / 2,160 / 2,462 / 2,197 / 2,176 ms；**2,200 ms** | 441.6 ms | 1,046.0 ms | 551 ms | 956 ms | 234 / 379 ms | 304 ms | 376 / 119 |
| `columnsize=0` | 2,710 / 2,137 / 2,196 / 2,232 / 2,067 / 2,099 / 2,015 / 2,156 / 2,031 ms；**2,137 ms** | 428.0 ms | 1,044.1 ms | 552 ms | 972 ms | 238 / 374 ms | 295 ms | 376 / 119 |

九轮首扫中位数快约 2.9%；`movie_item_insert` 累计中位数快约 3.1%。无变化重扫慢约 1.7%、前台 p95 慢约 1.7%，仍低于 5% 回退门；target 基本持平，目录列表 p95 和 batch p95 改善。反向运行的三组配对首扫都更快；九轮中候选有一轮 2,710 ms、基线有一轮 2,462 ms 明显偏慢，故保留原始分布并以中位数报告。SQLite DML 仍为 119，批次数不变。migration 会一次性重建现有 FTS 索引；升级回归确认 title、独立 sort title、original title、alias 均仍可搜索，且重建后没有 `media_search_docsize` 表。PostgreSQL 不使用此 migration；SQLite 中位数 2,137 ms 仍高于 LUX-270 的 2,018 ms 参考，LUX-275 阶段门继续开放。本机 ARM64 结果不外推 NAS/x86_64。

### LUX-275 SQLite PRAGMA 与 16k discovery 批次实验（未保留）

2026-09-27 使用 Apple M4 / 16 GiB / ARM64、SHA-256 为 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914` 的 60,000 文件 / 600 目录 fixture，SQLite `synchronous=FULL`、锁采样关闭。固定 release 基准二进制交错运行；PRAGMA 候选统一设置到连接池每条 SQLite 连接。基准现在接受 `LUX_PERF_SQLITE_CACHE_KIB`、`LUX_PERF_SQLITE_WAL_AUTOCHECKPOINT_PAGES` 和 `LUX_PERF_SQLITE_TEMP_STORE`，并报告实际值及首扫后 WAL 文件大小。SQLite [`cache_size`](https://sqlite.org/pragma.html#pragma_cache_size) 为每连接页缓存；[`wal_autocheckpoint`](https://www.sqlite.org/wal.html#automatic_checkpoint) 默认在约 1,000 页后触发自动 checkpoint；[`temp_store`](https://sqlite.org/pragma.html#pragma_temp_store) 控制临时表和索引的存放位置。

| SQLite 设置 | 首扫索引 | 无变化重扫 | 前台 / 目录列表 p95 | batch p95 | 首扫后 WAL 文件 | 结果 |
|---|---:|---:|---:|---:|---:|---|
| 每连接 cache 2 MiB / 默认 | 2,786 / 2,237 / 2,271 / 2,325 / 2,234 / 2,179 / 2,230 / 2,320 ms；中位数 **2,254 ms** | 928 ms | 238 / 366 ms | 310 ms | 约 17 MiB | 基线 |
| 每连接 cache 4 MiB | 2,209 / 2,200 / 2,217 / 2,168 / 2,201 / 2,216 / 2,211 / 2,208 ms；中位数 **2,209 ms** | 979 ms | 239 / 386 ms | 305 ms | 约 17 MiB | 首扫快约 2.0%，无变化重扫慢约 5.4%、目录列表 p95 慢约 5.3%；不采用 |
| `wal_autocheckpoint=1000` | 2,767 / 2,318 / 2,332 ms；中位数 **2,332 ms** | 933 ms | 241 / 366 ms | 321 ms | 17.2 MiB | 默认值 |
| `wal_autocheckpoint=5000` | 2,338 / 2,262 / 2,313 ms；中位数 **2,313 ms** | 949 ms | 238 / 381 ms | 334 ms | 33.8 MiB | 首扫快约 0.8%，没有稳定收益 |
| `wal_autocheckpoint=10000` | 2,302 / 2,286 / 2,291 ms；中位数 **2,291 ms** | 944 ms | 236 / 338 ms | 337 ms | 49.1 MiB | 首扫快约 1.8%，WAL 接近增至 3 倍；不采用 |
| `temp_store=DEFAULT` | 2,555 / 2,178 / 2,124 ms；中位数 **2,178 ms** | 957 ms | 235 / 372 ms | 304 ms | 17.2 MiB | 默认模式 |
| `temp_store=MEMORY` | 2,284 / 2,326 / 2,322 ms；中位数 **2,322 ms** | 1,022 ms | 236 / 462 ms | 350 ms | 17.2 MiB | 首扫慢约 6.6%、目录列表 p95 慢约 24%；不采用 |

另将 discovery work unit、正向文件批次和 observation 上限从 8k / 8,192 提至 16k / 16,384，目录组从 80 提至 160。三组交错 SQLite 运行的首扫：8k 为 2,265 / 2,144 / 2,267 ms（中位数 2,265 ms），16k 为 2,269 / 2,195 / 2,200 ms（中位数 2,200 ms）；配对差异不稳定。正向事务数从 8 降至 4，SQL/DML 从 376/119 降至 296/103；代价是无变化重扫从 996 增至 1,110 ms（慢约 11.4%）、batch p95 从 308 增至 588 ms（长约 91%），目录列表 p95 从 400 增至 429 ms。候选未通过延迟回退门，正式预算恢复为 8,000 文件 / 8,192 observation。

这些结果只代表本机 ARM64；没有把试验 PRAGMA 或 16k 批次留在运行时代码，LUX-275 严格性能门仍开放。

### LUX-275 扫描 CPU、复核、SQLite 调优与双缓冲候选

2026-09-27 对同一 Apple M4 / 16 GiB / ARM64、60,000 文件 / 600 目录 fixture（SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`）逐项评估。基准是一个扫描作业，扫描内部准备并发峰值为 9、目录 reader 并发为 1；另外在扫描期间发出 50 个前台请求。这不是多个扫描客户端同时并发。除特别说明外，SQLite 使用 `synchronous=FULL`，两后端均使用 release 基准二进制。

#### 目录级 Provider ID 解析缓存（保留）

Lite 扫描现在为每个目录共享一个惰性 `OnceLock`，由有界文件准备任务中的首个电影文件解析父目录名；同目录后续电影不再重复解析。文件名中的 Provider ID 仍优先于目录标签，目录标签只补缺失项。`tests/scanning_jobs.rs` 覆盖目录 ID 继承及文件标签覆盖；库级测试覆盖合并规则。

同机交错三轮 A/B：SQLite 首扫中位数 2.280 → 2.188 秒（约快 4.0%），正向准备累计中位数 696 → 563 ms；PostgreSQL 首扫中位数 6.075 → 6.111 秒（约慢 0.6%，处于波动范围），正向准备累计中位数 778 → 583 ms。无变化重扫中位数分别为 SQLite 996 → 976 ms、PostgreSQL 3.360 → 3.130 秒；前台 p95 分别为 237 → 240 ms、271 → 275 ms，均未超过 5% 回退线。SQL/DML 与 8 个正向提交批次不变。保留该改动是因为两个后端都明显减少了准备工作，SQLite 端到端也有收益，PostgreSQL 没有出现超过回退门槛的回退；这不表示 PostgreSQL 首扫已被稳定加速。

#### 跳过提交前逐文件二次 stat（拒绝）

一次 60k 测量中，`positive_file_recheck` 累计约为 SQLite 123 ms、PostgreSQL 132 ms。目录句柄或父目录 mtime 不能证明每个文件仍是读目录时的同一个观察结果：文件内容和大小可以在父目录 mtime 不变时改变。新增单元测试 `manifest_directory_batch_stat_detects_file_changes_without_directory_mtime_change` 验证该竞争窗口，当前逐文件复核继续保留。该测试量化的是本机本地文件系统；没有据此推断 SMB/NFS 的 RTT 成本。

#### SQLite cache、temp store 与 mmap PRAGMA（不进入运行时）

在默认设置对照上分别测试 64 MiB `cache_size`、`temp_store=MEMORY` 和 256 MiB `mmap_size`。64 MiB cache 的首扫中位数为 2.173 → 2.156 秒，配对中位数收益仅约 0.55%；无变化重扫 995 → 940 ms，前台 p95 235 → 240 ms，首扫后 WAL 文件中位数约 17.2 → 26.0 MiB。cache 上限按连接生效，扩大后还会乘以池内连接数；收益不足以抵消内存成本。`temp_store=MEMORY` 的既有三轮结果是首扫慢约 6.6%、目录列表 p95 慢约 24%。256 MiB mmap 的首扫中位数 2.206 → 2.339 秒（慢约 5.8%），没有通过首扫门槛。

没有采用这些运行时 PRAGMA。性能测试保留 `LUX_PERF_SQLITE_MMAP_SIZE` 环境变量，以便复现 mmap 单项实验；该测试开关不影响数据库默认配置。

#### 跳过电影父目录 refresh UPDATE（不实施缓存）

`refresh_existing_movie_parent_folders_in_transaction` 已在单事务内去重目录，并用 `WHERE` 排除值未变化的行；同批新插入目录也不会重复 refresh。8 个正向提交批次中，整个目录 refresh 阶段累计中位数约 SQLite 45 ms、PostgreSQL 78 ms，其中包含目录查询和实际更新，真正可省略的无变化 UPDATE 更少。跨事务 `verified_folders` 集合又可能被并发增量扫描写入变旧，因此不引入缓存和失效协议。

#### 粗粒度双缓冲（拒绝）

候选按 8,000 文件保留正向提交边界，最多只让一个数据库提交任务在途；读者准备下一缓冲区时，前一缓冲区进行最后的文件复核和事务提交。每组按基线、候选交错运行，三轮完整首扫索引如下：

| 后端 | 基线三轮 / 中位数 | 双缓冲三轮 / 中位数 | 提交批次 / SQL / DML | 无变化重扫中位数 | 前台 p95 / batch p95 中位数 |
|---|---:|---:|---:|---:|---:|
| SQLite | 2,690 / 2,192 / 2,161 ms；**2,192 ms** | 2,182 / 2,199 / 2,181 ms；**2,182 ms** | 8 批；376 / 119（候选个别轮 SQL 计数为 378，DML 不变） | 995 → 967 ms | 245 → 235 / 306 → 305 ms |
| PostgreSQL | 5,780 / 5,946 / 5,859 ms；**5,859 ms** | 5,815 / 6,048 / 5,953 ms；**5,953 ms** | 8 批；344 / 95 | 3,233 → 3,096 ms | 307 → 280 / 797 → 789 ms |

SQLite 的原始中位数差约 0.5%，但三组配对中候选有两组略慢，属于噪声；PostgreSQL 三组配对首扫都略慢，原始中位数慢约 1.6%。PostgreSQL `positive_commit` 累计中位数约 4.611 → 4.586 秒，没有显示事务阶段被稳定缩短。重扫和前台指标虽未超 5% 回退线，但不足以抵消首扫没有改善的结果，因此双缓冲没有保留在运行时代码。该轮 PostgreSQL 锁采样关闭，不能据此声称锁等待为零；WAL 是临时测试容器的统计值，候选中位数约高 0.2%。

#### 严格电影/剧集库文件名只解析一次（保留）

严格 `MOVIE` / `SERIES` 库原先先在扫描驱动循环中解析文件名以分类，再在有界准备任务中解析一次以构造媒体记录。现在分类模式交给准备任务；任务解析一次后，把同一个 `ParsedMovieFilename` / `ParsedEpisodeFilename` 传给记录构造器。无法解析的严格库媒体仍按原语义进入 `Unresolved`；混合库继续使用原有预分类。没有改变有界并发、目录读取或 8,000 文件提交批次。

固定 release 二进制在同一 60,000 文件 / 600 目录 fixture 上交错各跑三轮。SQLite 使用 `synchronous=FULL` 并关闭锁采样；PostgreSQL 16.15 每轮使用临时空库，关闭锁采样以减少基准扰动。每轮包括首扫、120,000 个 target、无变化重扫和 50 个前台请求。结果：

| 后端 | 首扫索引中位数 | target 中位数 | 首扫索引 + target | 无变化重扫 | 前台 p95 | batch p95 | DML / 正向提交批次 |
|---|---:|---:|---:|---:|---:|---:|---:|
| SQLite | 2,161 → 1,947 ms（快 9.9%） | 561 → 619 ms（慢 10.3%） | 2,722 → 2,566 ms（快 5.7%） | 1,060 → 1,041 ms | 269 → 260 ms | 298 → 270 ms | 119 → 119 / 8 → 8 |
| PostgreSQL | 5,839 → 5,767 ms（快 1.2%） | 2,416 → 2,484 ms（慢 2.8%） | 8,331 → 8,251 ms（快 1.0%） | 3,276 → 3,164 ms | 289 → 281 ms | 782 → 789 ms | 95 → 95 / 8 → 8 |

三轮原始值（基线 → 候选，单位 ms）：SQLite 首扫索引 `2394/2150/2161 → 1946/1960/1947`，target `587/557/561 → 707/599/619`，无变化重扫 `1094/1060/994 → 1041/1076/1030`，前台 p95 `330/269/255 → 261/260/246`，batch p95 `348/298/298 → 268/278/270`。PostgreSQL 首扫索引 `5803/5915/5839 → 5802/5767/5620`，target `2566/2416/2413 → 2624/2484/2344`，无变化重扫 `3463/3236/3276 → 3164/3080/3245`，前台 p95 `320/289/286 → 279/281/290`，batch p95 `782/785/766 → 807/789/759`。

阶段计时显示，严格库的串行 `positive_classification` 累计中位数从 SQLite 311 ms、PostgreSQL 312 ms 降到约 1 ms；准备阶段墙钟累计中位数分别从 600 → 366 ms、616 → 394 ms。PostgreSQL 的数据库事务仍占主要时间，所以首扫只改善约 1.2%。SQLite target 单项慢约 10.3%，但与首扫合计仍快约 5.7%；该阶段没有被此代码直接修改，原因尚不能从当前样本确定，后续比较须继续观察。DML 与提交批次不变；SQLite SQL 数为基线 `376/376/376`、候选 `372/377/372`，PostgreSQL 为 `344/344/342 → 344/344/342`。SQLite WAL 文件字节数中位数 `17,168,072 → 17,122,752`；PostgreSQL WAL 中位数 `222,521,670 → 222,533,471`。本组关闭了 PostgreSQL 锁采样，不用其锁等待字段推断实际等待情况。

把分类与解析结果改成互斥的 `Movie(parsed)` / `Episode(parsed)` / `Unresolved` 类型后，额外同机配对复测为 SQLite 2,219 → 2,033 ms、PostgreSQL 6,028 → 5,826 ms；这只是最终类型形态的单组确认，不代替上表三轮。另有一个文件系统缓存较冷的候选单轮为 2,415 ms，其 `directory_open` / `directory_readdir` 累计耗时为 253 / 182 ms；紧邻的基线与候选复测分别为 2,219 / 2,033 ms，因而保留这次偏慢样本并以三轮中位数作结论。

这几项局部实验都不关闭 LUX-275：阶段门仍要求在完整最终路径上证明双后端稳定首扫收益，并满足安全回归与完整检查。

### SQLite 跳过重复 original title FTS tokens（未保留）

2026-09-27 评估候选 migration `0149_skip_redundant_original_title_fts_tokens.sql`：当 `original_title` 与 `title` 仅有 ASCII 大小写差异时，SQLite 搜索 trigger 不再重复写入同一组 FTS tokens；真正不同的原文标题仍索引。回归覆盖迁移升级、INSERT、UPDATE、相同标题仍可搜索及不同 original title 仍可搜索。PostgreSQL 路径未修改。

基线从干净提交 `275fe6c9` 构建，候选与基线源码提交相同，仅额外包含 migration 0149。Apple M4 / 16 GiB / ARM64，fixture SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`，60,000 文件 / 600 目录；SQLite 使用 `synchronous=FULL` 并关闭锁采样，PostgreSQL 16 每轮使用一次性空库并保留锁采样。固定 release 二进制交错运行各三轮：

| 后端/版本 | 首扫索引：三轮 / 中位数 | 120k target | 无变化重扫 | 前台 p95 / 目录列表 p95 | batch p95 | SQL / DML | WAL / 最大锁等待者 |
|---|---:|---:|---:|---:|---:|---:|---:|
| SQLite 基线 | 2,488 / 1,955 / 1,942 ms；**1,955 ms** | 623 ms | 1,035 ms | 249 / 370 ms | 280 ms | 370 / 119 | 17,172,192 bytes |
| SQLite 候选 0149 | 1,941 / 1,960 / 1,933 ms；**1,941 ms** | 656 ms | 1,028 ms | 239 / 363 ms | 268 ms | 372 / 119 | 16,541,832 bytes |
| PostgreSQL 基线 | 5,732 / 5,735 / 5,871 ms；**5,735 ms** | 2,471 ms | 3,136 ms | 287 / 495 ms | 781 ms | 344 / 95 | 221,585,677 bytes / 0 |
| PostgreSQL 候选 0149 | 5,635 / 5,760 / 5,603 ms；**5,635 ms** | 2,539 ms | 3,089 ms | 275 / 467 ms | 752 ms | 344 / 95 | 221,551,625 bytes / 0 |

SQLite 首扫中位数只快 14 ms（约 0.7%）；两组暖缓存配对分别约慢 0.3% 和快 0.5%，不能证明端到端提速。候选 `movie_item_insert` 累计中位数快约 2.2%，`positive_index_apply` 累计中位数快约 1.3%，SQLite 首扫后 WAL 文件减少约 3.7%，但正向 DML 与批次未变；target 单项中位数增加约 5.3%。PostgreSQL 路径未改变，首扫差异处于运行波动范围。综合扫描耗时没有稳定收益，撤回 migration 0149 与兼容 trigger 修改；保留本节数据，LUX-275 性能门继续开放。本机结果不外推 NAS/x86_64。

### LUX-275 准备任务上下文与路径复用 A/B（未保留）

2026-09-27 在 `96e77824` 基线上评估缩减逐文件准备任务开销的几个窄候选。使用相同 Apple M4 / 16 GiB / ARM64、60,000 文件 / 600 目录 fixture；SQLite 与 PostgreSQL 每轮使用固定 release 二进制交错运行。Arc 候选三轮，组合路径/文件类型候选六轮，路径单项候选三轮。

| 候选 | SQLite 首扫中位数 | PostgreSQL 首扫中位数 | 其他观测 | 决定 |
|---|---:|---:|---|---|
| `Arc` 共享 root/path 上下文 | 1,947 → 1,963 ms（慢约 0.8%） | 5,608 → 5,655 ms（慢约 0.8%） | 两边准备阶段累计时间都增加 | 撤回 |
| 预计算 path + 文件类型布尔值 | 1,955 → 1,918 ms（快约 1.9%） | 5,660.5 → 5,628.5 ms（快约 0.6%） | SQLite 前台 p95 238 → 251.5 ms，回退约 5.7%，超过门槛 | 撤回组合候选并拆项 |
| 只复用预计算 path | 2,055 → 1,969 ms（快约 4.2%） | 5,678 → 5,695 ms（慢约 0.3%） | SQLite 三组首扫配对有两组改善、一组回退；无变化重扫 979 → 1,022 ms。PG 首扫配对方向不一致；DML 与 8 个正向批次不变，SQL 计数基本持平 | 不保留，未形成双后端稳定收益 |

这些候选没有改变扫描任务粒度。提出的“把每文件 JoinSet 任务改成 500–1,000 文件一组的 `spawn_blocking`/Rayon CPU 分块”当时没有直接 A/B；后续对 500 文件 `spawn_blocking` 分块的测试结果见下节。已保留的 `275fe6c9` 是另一项优化：严格电影/剧集文件名在准备任务中只解析一次，并把解析结果复用于索引记录构造；它没有批量化 JoinSet 任务。LUX-275 阶段门继续开放。

### LUX-275 分块 CPU 准备任务 A/B（未保留）

2026-09-27 直接评估将严格电影/剧集库中每个待处理文件的 Tokio `JoinSet` 任务，改为每 500 个文件一组的有界 `spawn_blocking` CPU 准备任务；`.strm` 读取、路径安全校验和混合库分类路径保持异步原流程。基线与候选都是同一源提交 `a06907b4` 的固定 release 构建，候选二进制仅包含未提交的分块实现。基线 SHA-256 `7a8507bbfb8c14c40ed40538e9d9dfd2c5931955f5c8f76ed88ef0c5603ac7e4`，候选 SHA-256 `640c541eb15363970ffac40c97b9d236b4cdc2e8b6f947f3f5fce83cbd0edc31`。

同一 Apple M4 / 16 GiB / ARM64 主机，确定性 60,000 文件 / 600 目录 fixture（SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`），基线与候选交错各运行三轮。SQLite 使用 `synchronous=FULL` 并关闭锁采样；PostgreSQL 16 每轮使用独立空数据库并开启锁采样。每轮运行 `performance` 集成测试中的 `lux_270_manifest_job_scan_benchmark`；三轮原始结果按“索引 / 120k target / 无变化重扫”列出：

| 后端/版本 | 首扫索引：三轮 / 中位数 | 120k target 中位数 | 无变化重扫：三轮 / 中位数 | 前台 / 目录列表 p95 中位数 | batch p95 中位数 | SQL / DML | 正向提交批次 | WAL 中位数 / 最大锁等待者 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| SQLite 基线 | 1,932 / 1,958 / 1,911 ms；**1,932 ms** | 614 ms | 1,022 / 1,026 / 1,050 ms；**1,026 ms** | 236 / 365 ms | 265 ms | 372 / 119 | 8 | - |
| SQLite 分块候选 | 1,849 / 1,837 / 1,858 ms；**1,849 ms** | 572 ms | 1,035 / 1,005 / 954 ms；**1,005 ms** | 238 / 355 ms | 265 ms | 372 / 119 | 8 | - |
| PostgreSQL 基线 | 5,568 / 5,570 / 5,591 ms；**5,570 ms** | 2,443 ms | 3,353 / 3,085 / 3,032 ms；**3,085 ms** | 272 / 316 ms | 737 ms | 344 / 95 | 8 | 221,998,535 bytes / 0 |
| PostgreSQL 分块候选 | 5,597 / 5,781 / 5,726 ms；**5,726 ms** | 2,477 ms | 3,075 / 3,114 / 3,012 ms；**3,075 ms** | 275 / 318 ms | 771 ms | 344 / 95 | 8 | 226,764,773 bytes / 0 |

SQLite 首扫三组配对分别改善约 4.3%、6.2%、2.8%；PostgreSQL 三组分别回退约 0.5%、3.8%、2.4%。候选把 `positive_prepare_wall` 调用数从 601 降到 9，并记录到 120 个 CPU 分块覆盖 60,000 个文件，但 PostgreSQL 的索引中位数仍慢约 2.8%，batch p95 也从 737 增至 771 ms。重扫、前台 p95、DML 数量和 8 个正向批次未明显退化；SQLite 的稳定收益不足以抵消 PostgreSQL 的稳定回退，故撤回通用分块改动，不作正式采纳。候选实现的 `scanning_jobs` 测试两次均为 75 passed，release performance 测试成功编译；性能门未通过后已撤回候选代码。当前扫描代码继续使用有界逐文件 `JoinSet`；保留的 `275fe6c9` 仍是文件名重复解析优化。结果只代表本机 ARM64，不外推 NAS/x86_64；LUX-275 阶段门继续开放。

### SQLite 文件系统 claim 无 RETURNING 快路径（保留；PostgreSQL 不启用）

2026-09-27 针对首扫的 `claim_manifest_add_filesystem_entries_in_transaction` 做优化。SQLite 上先在 savepoint 内批量插入且不取回 `RETURNING` 行；如果每个 chunk 的 `rows_affected` 都等于输入行数，就直接以输入路径作为已 claim 集合。若任何 chunk 有部分冲突，则回滚整个试插并重跑原有精确 `RETURNING relative_path` 查询，只有真正插入的路径会继续建源和媒体条目。PostgreSQL 保持原 `RETURNING` 路径，因为同一快路径没有证明该后端有稳定收益。

在同一 Apple M4 / 16 GiB / ARM64 主机和相同的确定性 60,000 文件 / 600 目录 fixture（SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`）上，使用固定 release 二进制交错运行五轮。SQLite 为 `synchronous=FULL`、锁采样关闭；PostgreSQL 16.15 每轮使用新的空数据库并采样锁。基线二进制 SHA-256 `a4f35bb380a369629bed454be86f210e626bf1d36eae4774e1f017ab8f01bc80`，候选 SHA-256 `7db1b2f3557aec073ff671f23b2b85ba096e4db826a25f40f22d7b5b0fa16d98`；两者源提交标记均为 `3b476d54`，候选含未提交的 SQLite 快路径。

| 后端/版本 | 首扫索引：五轮 / 中位数 | 120k target 中位数 | 索引 + target 中位数 | 无变化重扫 | 前台 / 目录列表 p95 | batch p95 | SQL / DML | 正向批次 | WAL / 最大锁等待者 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| SQLite 基线 | 2,673 / 1,990 / 2,051 / 1,981 / 1,996 ms；**1,996 ms** | 597 ms | 2,604 ms | 1,037 ms | 238 / 373 ms | 284 ms | 376 / 119 | 8 | - |
| SQLite 候选 | 1,915 / 1,938 / 1,920 / 1,877 / 1,877 ms；**1,915 ms** | 614 ms | 2,526 ms | 1,037 ms | 240 / 375 ms | 268 ms | 388 / 119 | 8 | - |
| PostgreSQL 基线 | 5,662 / 6,043 / 5,794 / 5,661 / 5,765 ms；**5,765 ms** | 2,445 ms | 8,171 ms | 3,154 ms | 272 / 363 ms | 766 ms | 344 / 95 | 8 | 221,615,287 bytes / 0 |
| PostgreSQL 候选 | 5,602 / 5,805 / 5,664 / 5,708 / 5,916 ms；**5,708 ms** | 2,441 ms | 8,164 ms | 3,206 ms | 281 / 325 ms | 763 ms | 344 / 95 | 8 | 221,715,426 bytes / 0 |

SQLite 首扫中位数快约 4.1%，五组配对候选都更快；`positive_add_filesystem_claim` 累计中位数从 226 ms 降至 167 ms（快约 26%）。首扫加 120k target 的中位数快约 3.0%；无变化重扫相同，前台 p95 高约 0.8%，batch p95 更低。SQLite SQL 语句中位数多 12 条（savepoint/release 控制语句），DML 与正向批次不变。首轮基线有较冷的离群值，但五轮中位数及配对方向均支持保留。

PostgreSQL 路径仍使用原 `RETURNING` 查询；五轮首扫中位数差约 1%，配对方向混合，按持平处理。无变化重扫回退约 1.6%，前台 p95 回退约 3.3%，均低于 5% 门槛；batch p95、DML、正向批次和 WAL 基本持平，最大锁等待者为 0。因此只在 SQLite 启用快路径，不给 PostgreSQL 增加 savepoint 开销。`scanning_jobs` 全套 75 项通过，候选 release performance 测试编译并实跑；本实验不关闭 LUX-275 整体阶段门，ARM64 数字不外推 NAS/x86_64。

### 混合库分类复用已解析文件名

严格 `MOVIE` / `SERIES` 库此前已由 `275fe6c9` 把解析结果从分类模式交给准备阶段；混合库仍在分类时解析 episode/movie 以决定类型，随后准备阶段再次解析同一文件名。现在混合 Manifest 分类携带 `ParsedMovieFilename` / `ParsedEpisodeFilename`，准备阶段直接复用。旧 Manifest 恢复分类路径保持原逻辑；电影/剧集严格库路径、文件类型判定、NFO 优先级和未解析语义不变。

为覆盖此路径，`lux_270_manifest_job_scan_benchmark` 增加 `LUX_PERF_LIBRARY_KIND=mixed` 选项，默认仍为 `movie`。在同一 Apple M4 / 16 GiB / ARM64 和确定性 60,000 文件 / 600 目录 fixture（SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`）上，固定 release 二进制交错跑三轮；每个样本都新建数据库，PG 用独立空库。基线二进制 SHA-256 `13726dfbd715245a92466725b83564a15078096e708f59c81b668c6d9e021f38`，候选 SHA-256 `b8d3908ae19a9029dc8266b47a7013554089170a4e976e608338e0681a51cf37`。fixture 全是可解析电影名，但扫描库类型设为 `MIXED`，用以测量混合分类路径。

| 后端 | 首扫索引：交错三轮 / 中位数 | 分类累计工作时间 | 文件准备累计工作时间 | 无变化重扫 | 前台 p95 | batch p95 | SQL / DML / 正向批次 |
|---|---:|---:|---:|---:|---:|---:|---:|
| SQLite 基线 | 3,240 / 2,835 / 2,839 ms；**2,839 ms** | 998 ms | 467 ms | 1,002 ms | 239 ms | 404 ms | 392 / 119 / 8 |
| SQLite 候选 | 2,804 / 2,824 / 2,786 ms；**2,804 ms** | 968 ms | 223 ms | 987 ms | 244 ms | 385 ms | 392 / 119 / 8 |
| PostgreSQL 基线 | 6,768 / 6,754 / 6,771 ms；**6,768 ms** | 980 ms | 458 ms | 3,094 ms | 269 ms | 957 ms | 346 / 95 / 8 |
| PostgreSQL 候选 | 6,524 / 6,753 / 6,644 ms；**6,644 ms** | 955 ms | 223 ms | 3,134 ms | 271 ms | 881 ms | 344 / 95 / 8 |

解析结果复用使 `positive_file_prepare` 累计工作时间在 SQLite / PostgreSQL 分别下降约 52% / 51%；首扫索引中位数改善约 1.2% / 1.8%。SQLite 三组配对都更快；PostgreSQL 两组更快、一组近乎持平。无变化重扫和前台 p95 差异均低于 5%，DML 与 8 个正向提交批次不变。总耗时收益有限，因为数据库阶段仍占主要时间，但准备阶段 CPU 工作稳定减少且没有后端回退，故保留。集成测试覆盖混合库电影、分集、NFO 分类与 unresolved 行为；LUX-275 整体阶段门仍开放。

### PostgreSQL 文件系统 claim 无 RETURNING 快路径（未保留）与目录 key 去重评估

2026-09-27 在 Apple M4 / 16 GiB / ARM64 上，以同一 60,000 文件 / 600 目录 fixture（SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`）对 PostgreSQL 16 做五组交错首扫。每轮使用新的空数据库；固定 release 二进制的源提交标记均为 `573de379`。基线 SHA-256 `ce831437d0374973077fb8de067e894c6eabfd838eea5bd60b5668ded90c2a6f`，候选 SHA-256 `e67ec90428667f1b2b9c2c958c156fbc4f36978539c7099c235de81099ea6643`；候选仅额外启用 PostgreSQL 无 `RETURNING` claim 快路径。

| PostgreSQL 16 指标 | 基线：五轮 / 中位数 | 候选：五轮 / 中位数 |
|---|---:|---:|
| 首扫索引完成 | 5,675 / 6,036 / 5,750 / 5,674 / 5,602 ms；**5,675 ms** | 5,623 / 5,584 / 5,665 / 5,788 / 5,666 ms；**5,665 ms** |
| 正向提交阶段 | **4,661 ms** | **4,614 ms** |
| `positive_add_filesystem_claim` 累计工作时间 | **1,024.8 ms** | **994.8 ms** |
| 无变化重扫 | **3,104 ms** | **3,135 ms** |
| 前台 p95 / batch p95 | 272 / 774 ms | 278 / 751 ms |
| SQL / DML / 正向提交批次 | 344 / 95 / 8 | 360 / 95 / 8 |
| WAL 中位数 / 最大锁等待者 | 224,888,364 bytes / 0 | 221,608,151 bytes / 0 |

claim 子阶段约快 2.9%，但首扫中位数只快 10 ms（约 0.2%），五组配对有三组改善、两组回退；候选每批增加 savepoint 控制语句，SQL 中位数增加 16 条，DML 与提交批次不变。重扫约慢 1%，前台 p95 约慢 2.2%，都在 5% 观察门内。未得到稳定的端到端收益，因此 PostgreSQL 保留原 `RETURNING` 实现；SQLite 已验证有效的快路径继续保留。新增 PostgreSQL 并发占位回归测试验证竞态中已有的增量记录不会被覆盖，且该路径不会错误地为扫描项创建 media source。

同一基线的 movie storage 阶段中位数为 `movie_folder_refresh` 77 ms、`movie_item_prefetch` 101 ms、`movie_item_insert` 1,807 ms。源代码确有多处重复目录切分与 identity-key 构造，但这些阶段计时含数据库操作，尚未单独测出字符串处理占比；当前证据不足以支持引入每批目录映射结构。目录 key 去重暂不实施，若后续继续优化，应先增加窄范围计时并证明它能带来超过噪声的全链路收益。以上只代表本机 ARM64 与本地 PostgreSQL 容器；LUX-275 阶段门仍开放。

### LUX-275 数据库减负候选：generation lookup、PG search FK 与 availability trigger

2026-09-27 对同一 Apple M4 / 16 GiB / ARM64 主机、PostgreSQL 16.15 本地容器、60,000 文件 / 600 目录 fixture（SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`）评估三个 PG 路径。每轮都使用新空数据库，执行 `lux_270_manifest_job_scan_benchmark` release 基准。SQLite 不受 PG 外键与 trigger migration 影响。

#### 移除 `media_search.item_id` 外键（保留）

PostgreSQL bootstrap 原有 `media_search.item_id REFERENCES media_items(id) ON DELETE CASCADE`。新 migration `0147_drop_media_search_item_fk.sql` 移除它；`media_items` 的 INSERT/UPDATE/DELETE statement triggers 仍负责创建、刷新和删除派生搜索行。真实 PG 回归确认 FK 已不存在、插入媒体项仍创建搜索行、删除媒体项仍由 DELETE trigger 清掉搜索行。候选与基线使用固定 release 二进制，SHA-256 分别为 `0d9c51a2c890f3ad86343e08b5829dcb55b863d6ed0c2c530e0f6915e1ab5bbe` 与 `449401934b95952d41b7547822b78b21d900fca58803bc4db6626f97aeacbc3d`；三轮按基线/候选交错运行：

| PostgreSQL 指标 | FK 保留：三轮 / 中位数 | FK 移除：三轮 / 中位数 |
|---|---:|---:|
| 首扫索引完成 | 5,851 / 5,820 / 5,811 ms；**5,820 ms** | 5,602 / 5,388 / 5,493 ms；**5,493 ms** |
| `movie_item_insert` 累计时间 | 1,786 / 1,868 / 1,926 ms；**1,868 ms** | 1,552 / 1,482 / 1,593 ms；**1,552 ms** |
| 120k target 物化 | 2,437 / 2,497 / 2,532 ms；**2,497 ms** | 2,429 / 2,415 / 2,444 ms；**2,429 ms** |
| 无变化重扫 | 3,164 / 3,186 / 3,260 ms；**3,186 ms** | 3,174 / 3,114 / 3,207 ms；**3,174 ms** |
| 前台 p95 / batch p95 | 271 / 797 ms | 277 / 721 ms |
| SQL / DML / 正向提交批次 | 344 / 95 / 8 | 344 / 95 / 8 |
| WAL 字节中位数 / 最大锁等待者 | 207,496,723 / 0 | 226,457,765 / 0 |

首扫三组配对都更快，中位数约快 5.6%；`movie_item_insert` 快约 16.9%。target、无变化重扫和前台 p95 均在 5% 观察门内，DML、提交批次和锁等待没有增加。WAL 字节样本中位数增加约 9.1%；这里读取的是 PostgreSQL 集群级 `pg_stat_wal` delta，当前实验不能把它归因于 FK 删除，故保留该差异作为需持续观察的指标。综合全链路和触发器回归，保留 PG 外键移除；直接 SQL 写入 `media_search` 若绕过媒体项 trigger，仍可能留下孤儿行，这是这项派生索引设计的完整性边界。

#### 跳过 `known_path_query` 与移除 generation lookup（未保留）

四轮原始 PG 基线的 `known_path_query` 累计中位数约 126 ms、每轮扫描中调用 9 次。该查询还识别已在同一 generation 提交的路径，防止事务重放再次增加根目录观测计数。试验将 ADD claim 前移，再对本事务已 claim 的路径跳过 lookup；`streamed_manifest_add_does_not_claim_a_concurrent_filesystem_entry` 回归失败，因为 claim 越过了 root checkpoint 后到达的增量写入，改变了当前“增量赢家”竞态顺序。要保留这个顺序并延后计数，需拆开 root checkpoint 并增加事务更新，估计会抵消最多约 126 ms 的节省。故不改变查询或写入顺序；新增 `manifest_discovery_does_not_count_a_path_already_seen_in_the_generation` 特征测试固定重放计数语义。

#### `media_sources_availability_ai` 额外早退（未保留）

`new_rows` 是 `media_sources` transition table，本身没有 `has_available_source`；正确的 guard 必须再次 join 父 `media_items`。已有 migration `0142_filter_available_source_promotions.sql` 已先筛出 `candidate.has_available_source = 0`，再 join `filesystem_entries`。临时 guard 在现有 UPDATE 前重复了这次父项查找。相对已移除 FK、未加 guard 的三轮，增加 guard 后首扫中位数 5,636 → 5,957 ms（慢约 5.7%），`movie_source_insert` 累计中位数 1,209 → 1,652 ms（慢约 36.6%）；首扫三组均回退。临时 migration 和测试已撤回，不把其结论与保留的 FK migration 混为一项。

#### SQLite `original_title == title`（未新增改动）

本请求的严格相等条件是此前 SQLite migration 0149 已评估条件（ASCII 大小写等价）的子集。该 A/B 首扫仅快约 0.7%、仍落在运行噪声内，target 中位数还增加约 5.3%；既有测试已覆盖相同标题可搜索和独立 original title 搜索。未重复增加 migration 或改动 FTS trigger。

以上 PG 数据仅代表本机 ARM64 和本地容器，不外推 NAS/x86_64；LUX-275 双后端阶段门仍开放。

### LUX-275 空 baseline 快路径、状态查询合并与电影批次分配评估

2026-09-27 在 Apple M4 / 16 GiB / ARM64 上，以同一 60,000 文件 / 600 目录 fixture（SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`）评估首次扫描的空 baseline 查询和 manifest/job 状态读取。SQLite 使用 FULL synchronous；PostgreSQL 为本机 PostgreSQL 16 临时容器。每组固定 release 基准三轮，包含 120k targets、无变化重扫和扫描期间的 50 个并发前台 API 请求。以下是顺序 A/B 中位数，不是交错运行；结果仅代表这台 ARM64 主机与本地数据库。

#### 空 root 跳过文件 baseline 查询（保留）

Lite session 初始化时为每个 root 执行一次 `SELECT 1 ... LIMIT 1`。若当时没有 `filesystem_entries`，该 root 的本次发现不再为每批文件执行大参数 `IN (...)` baseline 查询；root 有历史条目时仍走原路径。ADD claim、generation CAS 和事务内文件状态保护保持不变。SQLite root 探测、空/非空结果单测及全量扫描回归通过。

| 后端 | 指标 | 原实现中位数 | 空 baseline 快路径中位数 |
|---|---|---:|---:|
| SQLite | 首扫索引 / 无变化重扫 | 1,959 / 1,049 ms | 1,903 / 1,026 ms |
| SQLite | baseline 查询累计 / SQL / DML | 约 34 ms / 388 / 119 | 0 ms / 364 / 119 |
| SQLite | 前台 p95 / batch p95 | 254 / 269 ms | 251 / 264 ms |
| PostgreSQL | 首扫索引 / 无变化重扫 | 5,533 / 3,244 ms | 5,463 / 3,319 ms |
| PostgreSQL | baseline 查询累计 / SQL / DML | 约 99–103 ms / 344 / 95 | 0 ms / 330 / 95 |
| PostgreSQL | 前台 p95 / batch p95 | 289 / 749 ms | 297 / 739 ms |

空 root 的批次 baseline 阶段归零；全链路中位数 SQLite 快约 2.9%、PostgreSQL 快约 1.3%，SQL 分别减少 24 / 14 条，DML 不变。样本不是交错运行，PG 无变化重扫和前台 p95 也有小幅波动，因此把它视为低风险的确定性查询削减，不把轻微端到端差异单独宣称为稳定性能收益。新扫描仍是一个扫描作业；“50 并发”指另行施加的前台 API 请求，不是 50 个扫描客户端。

#### 合并事务内重复的 active-job 状态查询（保留）

discovery chunk 事务开头的 manifest 查询现在同时读取 `library_id`、`generation`、job `status` 和 `cancel_requested`，供后续正向应用校验使用，移除同一事务中重复的 active-job JOIN SELECT。事务末尾仍用 `UPDATE ... WHERE status = 'RUNNING' AND cancel_requested = 0` 检查任务是否仍接受发现；更新行数不为 1 时整笔事务回滚，因此取消或状态竞争的原子保护保留。性能计数器回归确认重复 SELECT 为 0。

| 后端 | 指标 | 合并前中位数 | 合并后中位数 |
|---|---|---:|---:|
| SQLite | 首扫索引 / SQL / DML | 1,903 ms / 364 / 119 | 1,886 ms / 358 / 119 |
| SQLite | 无变化重扫 / 前台 p95 / batch p95 | 1,026 / 251 / 264 ms | 1,020 / 245 / 261 ms |
| PostgreSQL | 首扫索引 / SQL / DML | 5,463 ms / 330 / 95 | 5,387 ms / 322 / 95 |
| PostgreSQL | 无变化重扫 / 前台 p95 / batch p95 | 3,319 / 297 / 739 ms | 3,180 / 274 / 713 ms |

该查询从每个正向提交批次一次降为零（本 fixture 为 8 次），DML 与 8 个提交批次不变。索引中位数变化分别约 −0.9% / −1.4%，幅度有限；保留的主要依据是移除确定冗余的事务内往返，并由最终有条件更新维持取消/状态边界。

#### 跨批次缓存已刷新的电影父目录（暂缓）

当前 `movie_folder_refresh` 整阶段计时约为 SQLite 46.5 ms、PostgreSQL 69.8 ms；该计时包含数据库工作，不能把它全部算作跨批次重复 CTE 的可省时间。把“本次扫描已验证”的 folder ID 缓存在事务之间，还需要证明不会跳过其他并发写入造成的 parent/provider 修复。当前没有单独量出无效果 UPDATE 的实际成本，也没有足够收益覆盖额外状态及其一致性边界，因此暂不引入跨批次缓存。

#### 合并新条目过滤前的临时 map/set（未保留）

试验仅对电影批次准备循环调整临时集合构造：新条目不再先进入 `parent_updates` / `provider_updates`，再由 `new_item_ids` 过滤。与前一候选相比，SQLite `movie_item_insert` 阶段中位数约从 437.1 降至 426.1 ms（省约 11 ms），但全链路首扫从 1,886 增至 1,932 ms（慢约 2.4%），batch p95 从 261 增至 302 ms。PostgreSQL `movie_item_insert` 基本不变（1,534.5 → 1,534.3 ms），首扫 5,387 → 5,338 ms 的约 0.9% 差异不足以排除运行噪声。候选已撤回；保留回归测试固定相同身份多来源仍合并 provider IDs，并按既有顺序确定 parent folder。

结论：生产路径保留空 baseline 快路径和 active-job SELECT 合并；父目录跨批次缓存暂缓；临时 map/set 改动未通过全链路与尾延迟观察门而撤回。性能对照为顺序 A/B，硬件、fixture 和数据库限定见本节开头；不外推到 NAS/x86_64。LUX-275 双后端阶段门仍开放。

### PostgreSQL presence ledger 与 target 外键优化

2026-09-27 在 Apple M4 / 16 GiB ARM64、本机 PostgreSQL 16.15 容器上，以相同 60,000 文件 / 600 目录 fixture（SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`）验证 PG 专属写路径。每轮使用新的空数据库；SQLite 未改变。

#### Presence ledger 单阶段更新（PG 保留，SQLite 保持原 SQL）

PG 的 `presence_ledger` 从 `matching` CTE 加 `id IN` 改为 `UPDATE ... FROM incoming`，把 ID、路径、指纹和 generation 条件放到同一更新连接中。三轮结果如下：

| PostgreSQL 指标 | 基线：三轮 / 中位数 | PG `UPDATE ... FROM`：三轮 / 中位数 |
|---|---:|---:|
| 首扫索引完成 | 5,143 / 5,154 / 5,166 ms；**5,154 ms** | 6,659 / 5,166 / 5,241 ms；**5,241 ms** |
| `presence_ledger` 累计工作时间 | **1,762 ms** | **1,544 ms** |
| 无变化重扫 | 3,056 / 3,081 / 3,151 ms；**3,081 ms** | 3,103 / 2,928 / 2,941 ms；**2,941 ms** |
| target / 前台 p95 / batch p95 | 2,348 / 280 / 688 ms | 2,428 / 288 / 721 ms |
| SQL / DML / PG 锁等待者 | 322 / 95 / 0 | 322 / 95 / 0 |

该 SQL 让 `presence_ledger` 累计时间下降约 12.4%，无变化重扫快约 4.5%；首扫中位数慢约 1.7%，batch p95 慢约 4.8%（接近观察门），均未越过 5% 门槛。保留它作为 PostgreSQL 的无变化重扫优化，不宣称首扫加速。把同一 `UPDATE ... FROM` 语句无条件用于 SQLite 曾使无变化重扫达到约 47.4 秒，因此实现按后端选择 SQL，SQLite 继续使用原查询。

#### 移除 `scan_job_targets.job_id` 外键（保留）

Migration `0148_drop_scan_job_targets_job_fk.sql` 删除批量 target 插入中逐行执行的父键探查。按之前的首轮 A/B，120k target 物化中位数从 2,454 ms 降至 1,960 ms。为排除当时前台 p95 的运行顺序干扰，又以独立新库交替做了三组配对：

| PostgreSQL 指标 | FK 保留：三轮 / 中位数 | FK 移除：三轮 / 中位数 |
|---|---:|---:|
| 首扫索引完成 | 5,182 / 5,239 / 5,238 ms；**5,238 ms** | 5,172 / 5,151 / 5,305 ms；**5,172 ms** |
| 120k target 物化 | 2,319 / 2,418 / 2,392 ms；**2,392 ms** | 1,914 / 1,882 / 2,005 ms；**1,914 ms** |
| 无变化重扫 | **2,832 ms** | **2,788 ms** |
| 前台 p95 / batch p95 | 279 / 694 ms | 280 / 698 ms |
| SQL / DML / 最大锁等待者 | 322 / 95 / 0 | 322 / 95 / 0 |

target 阶段三组都更快，中位数快约 20.0%；首扫、重扫、前台 p95 和 batch p95 中位数差异均小于 5%，SQL/DML 和 target 批次数不变。迁移移除的是数据库 FK，因此媒体库删除路径已在同一事务里显式清理对应 `scan_job_targets`；PostgreSQL 集成测试确认删库后没有孤儿 target。其他 target 清理仍走既有应用批量清理入口。

#### 未采纳：availability CTE 强制物化、unused `sort_title`

`unavailable_candidates AS MATERIALIZED` 候选相对现有 `0142` 过滤路径做三轮 A/B，试图强制先过滤 `has_available_source = 0` 的媒体项。针对性 `movie_source_insert` 阶段中位数从 1,177 ms 变为 1,211 ms（慢约 2.9%）；首扫仅快约 0.9%，重扫、尾延迟与 WAL 没有形成稳定改善，临时 migration 已撤回。现有 `0142` 已在连接 `filesystem_entries` 前表达父媒体项可用性过滤，不能仅凭 SQL 写法推断优化器一定做了额外探测。

`media_search.sort_title` 虽然不参与 PostgreSQL 搜索或排序，但把该列置空并绕过 sort-title-only upsert 的三轮对照，首扫中位数为 5,288 → 5,322 ms（候选慢约 0.6%），无变化重扫慢约 1.1%，target 快约 1.2%；WAL 集群计数较低但波动较大。没有可重复的端到端收益，PostgreSQL 0149 候选及相关测试已撤回。

附件提出的 `UPDATE OF is_missing` 不能与 transition table 同时用于 PostgreSQL statement trigger：`CREATE TRIGGER` 文档明确禁止在请求 transition relations 时指定 update 列表。[PostgreSQL 16 CREATE TRIGGER 文档](https://www.postgresql.org/docs/16/sql-createtrigger.html)。因此继续使用现有 statement trigger 和 transition table；若后续仍需跳过 generation-only 更新，应从更新语句形态或独立触发器设计入手，并单独测量。

以上测试只代表本机 ARM64 和 PostgreSQL 16 容器。LUX-275 首扫严格门仍开放；本轮 target 外键优化针对 target 阶段，不能替代 SQLite/PostgreSQL 的完整阶段门，也不能外推 NAS/x86_64。

### PostgreSQL `unixepoch()` 时钟调用与触发器候选

2026-09-27 在 Apple M4 / 16 GiB ARM64、本机 PostgreSQL 16 容器和相同 60,000 文件 / 600 目录 fixture（SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`）评估 PostgreSQL 时间函数、可用性触发器及 provider 索引触发器。每轮都用全新数据库运行 release `lux_270_manifest_job_scan_benchmark`。基线组先运行，候选组随后运行，因此这些结果是分组 A/B，而非交错 A/B。

#### `unixepoch()` 改用语句时间（保留）

PostgreSQL `unixepoch()` 原先在每次求值时调用 `clock_timestamp()`。migration `0149_statement_timestamp_unixepoch.sql` 将实现改为 `FLOOR(EXTRACT(EPOCH FROM statement_timestamp()))::BIGINT`，并标记 `PARALLEL SAFE`。保留 `FLOOR`，避免 BIGINT 转换四舍五入；使用 `statement_timestamp()`，避免 `CURRENT_TIMESTAMP` 把较长写事务里的后续 `updated_at` 固定到事务启动时刻。PostgreSQL 文档区分了事务起始时间、语句起始时间与调用时钟值。[PostgreSQL 16 日期与时间函数](https://www.postgresql.org/docs/16/functions-datetime.html)。SQLite 路径没有改变。PostgreSQL 集成测试确认了函数 volatility/parallel 属性、语句时间匹配，以及同一事务中跨语句时间可以前进。

| PostgreSQL 指标 | 原 `clock_timestamp()`：三轮 / 中位数 | `statement_timestamp()`：三轮 / 中位数 |
|---|---:|---:|
| 首扫索引完成 | 5,852 / 5,695 / 6,138 ms；**5,852 ms** | 5,505 / 5,384 / 5,275 ms；**5,384 ms** |
| 无变化重扫 | 3,255 / 2,966 / 2,996 ms；**2,996 ms** | 3,127 / 2,999 / 2,872 ms；**2,999 ms** |
| 120k target 物化 | 2,070 / 2,059 / 2,188 ms；**2,070 ms** | 1,828 / 1,900 / 1,996 ms；**1,900 ms** |
| 前台 p95 | 297 / 278 / 286 ms；中位数 **286 ms** | 357 / 279 / 272 ms；中位数 **279 ms** |
| batch p95 | 960 / 875 / 851 ms；中位数 **875 ms** | 761 / 739 / 720 ms；中位数 **739 ms** |
| SQL / DML | 322 / 95（中位数） | 322 / 95（中位数） |
| WAL 字节 | 221,591,746（中位数） | 222,426,002（中位数） |

首扫索引中位数快约 8.0%，target 物化快约 8.2%；无变化重扫持平，前台 p95、batch p95 与 WAL 未见回退。当前结果支持保留此 PG-only migration。收益比“百万次函数调用快 6.6 倍”的微基准比例小得多；对 Lux 的实际全链路结论以这里的端到端 fixture 为准。

#### 文件可用性 row trigger（未保留）

把带 transition table 的 statement trigger 换成 `AFTER UPDATE OF is_missing ... FOR EACH ROW WHEN (OLD.is_missing IS DISTINCT FROM NEW.is_missing)`，确实可避免 generation-only UPDATE 收集 transition table。但相同 60,000 条文件记录上的缺失/恢复压力测试显示，当前 statement trigger 将 `is_missing` 置 1 / 复原至 0 分别用时 3,226 / 3,285 ms；历史 row function 的候选在 120 秒 statement timeout 内仍未完成，最后整条语句回滚。逐文件函数重算 source availability，并对每个条目单独 UPDATE `media_items`，把 set-based 批处理退化为逐行派生索引工作。由于根移除可以一次影响大批条目，不能只按无变化重扫的收益决定改成 row trigger，因此没有迁移。

#### provider 空结果 guard（未保留）

候选 migration 在 `0146` 的 provider 索引 INSERT 周围增加 `IF EXISTS`，先检查 `new_rows` 是否含 provider IDs。与只含 `0149` 时的三轮全新数据库基线相比，guard 候选首扫索引中位数为 5,469 ms（基线 5,384 ms，慢约 1.6%），target 中位数 1,922 ms（基线 1,900 ms）；无变化重扫和尾延迟变化方向不一致。SQL 计数在 322–324 间波动、DML 均为 95，没有稳定减少。额外检查 transition table 没有带来可复现的全链路收益，临时 `0150` migration 已撤回；保留现有 materialized/filter 方案。

#### 合并路径 target INSERT（当前全量主路径已合并）

`insert_scan_manifest_postprocessing_targets_in_transaction` 已把当前 Manifest 全量扫描的 SOURCE / ITEM target 物化合并为一个 bounded-page CTE 和一条 INSERT。附件指出的两条重复 JOIN 位于通用 `record_scan_job_targets_in_transaction`，主要服务 reconciliation/path target 写入；本轮 60k Manifest 首扫的主 target 路径不走这两个重复 SELECT。没有为不影响该首扫瓶颈的兼容/增量路径改写 SQL；如后续以 path/reconciliation 批处理为目标，应单独量测该调用链并覆盖多 source 同 item 的去重语义。

LUX-275 双后端严格阶段门仍开放。以上数据库结果仅代表本机 ARM64 与 PostgreSQL 16 容器，不外推到 NAS/x86_64。

### PostgreSQL 扫描事务局部异步提交 A/B（保留）

2026-09-28 在本机 ARM64、PostgreSQL 16 本地容器，以固定 60,000 文件 / 600 目录 fixture（SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`）评估扫描事务局部 `SET LOCAL synchronous_commit = off`。基线与候选各运行六轮 release `lux_270_manifest_job_scan_benchmark`，每轮使用新数据库；候选只在 `begin_scan_write_transaction()` 的 PostgreSQL 事务中设置该值。该设置新增每事务一条 SQL，DML 数不变。

| PostgreSQL 指标 | 默认同步提交：六轮中位数 | 扫描事务局部异步提交：六轮中位数 |
|---|---:|---:|
| 首扫索引完成 | 5,599 ms | 5,236 ms |
| 120k target 物化 | 1,895 ms | 1,824 ms |
| 无变化重扫 | 3,032 ms | 3,027 ms |
| 前台请求 p95 | 290.5 ms | 292.5 ms |
| batch p95 | 783.5 ms | 719.5 ms |
| SQL / DML | 322 / 95 | 336 / 95 |
| WAL 字节 | 222,482,798（5 个有效基线样本） | 222,288,478 |

六轮汇总首扫中位数快约 6.5%，但样本受缓存变热和运行顺序影响。最后三轮采用反序交错运行，首扫中位数为 5,390→5,188 ms（快约 3.7%），三组中两组更快、一组慢约 3.8%；该子集更适合作为稳定收益估计。相同子集的 target 为 1,907→1,897 ms，重扫为 2,977→3,059 ms，前台 p95 为 292→292 ms，batch p95 为 733→731 ms。累计 `transaction_commit` 等待的中位数从 78.9 ms（5 个有效基线样本）降至 2.35 ms（6 个候选样本）；首个基线样本没有该阶段记录。WAL 与 DML 没有可辨变化。

PostgreSQL 的 `SET LOCAL` 在事务结束时恢复；metadata 写事务和 SQLite 不受影响。异步提交不会破坏数据库一致性，但异常退出可能丢失近期已确认而 WAL 尚未落盘的整笔事务，不只是相同时间长度对应的部分工作。PostgreSQL 16 文档说明默认 `wal_writer_delay = 200ms` 时，延迟上限可达三倍该值；具体丢失哪些事务取决于 WAL 刷盘时序。[WAL 配置文档](https://www.postgresql.org/docs/16/runtime-config-wal.html)；[`SET LOCAL` 文档](https://www.postgresql.org/docs/16/sql-set.html)。Lux 启动时会将未完成扫描标记取消，不自动续跑；异常退出后需重新发起扫描。基于扫描数据可通过重扫重建，保留此 PostgreSQL-only 局部设置。结果仅代表本机 ARM64 和本地 PostgreSQL 容器，不外推 NAS/x86_64，也不关闭 LUX-275 全阶段性能门。

### LUX-304 progressive poster workflow: 1k/10k SQLite/PostgreSQL A/B

2026-09-29 在 Mac16,10 / 16 GiB / ARM64（`uname -m=arm64`）交错运行基线与候选 release 性能测试。基线为 LUX-295 前的 `6424ab12`；`lux_270_manifest_job_scan_benchmark` 的扫描 A/B 候选是 `d88b46f6`，poster-worker 候选为 `d9ad36e3`（增加本批父目录路径缓存）。fixture 使用 `lux-catalog-fixture-v1`，1,000 文件 / 100 目录和 10,000 文件 / 200 目录，视频内容 SHA-256 均为 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`。SQLite 使用 `synchronous=FULL`、关闭 100 ms 锁采样；PostgreSQL 为 Docker 16.15，每轮新建 disposable 数据库并保留锁采样。用户目录 p95 为 50 个并发 `GET /api/v1/libraries/{id}/items` 请求；管理库列表 p95 单独统计。

`lux_270_manifest_job_scan_benchmark` 的中位数：

| 后端 / 文件数 | rounds B/C | 首扫索引 ms B→C | target ms B→C | 无变化重扫 ms B→C | 用户目录 p95 ms B→C | 管理库列表 p95 ms B→C | DML B/C |
|---|---:|---:|---:|---:|---:|---:|---:|
| SQLite / 1k | 8 / 8 | 48.5 → 48 | 7 → 7 | 255.5 → 325.5 | 42.5 → 46 | 206.5 → 268.5 | 37 / 37 |
| PostgreSQL / 1k | 8 / 8 | 166 → 170.5 | 29.5 → 29 | 319.5 → 391 | 72.5 → 77 | 234 → 290.5 | 37 / 37 |
| SQLite / 10k | 5 / 5 | 369 → 355 | 82 → 81 | 315 → 388 | 85 → 90 | 221 → 281 | 85 / 85 |
| PostgreSQL / 10k | 6 / 6 | 879 → 892.5 | 243.5 → 239 | 390.5 → 443 | 99 → 102.5 | 259 → 315.5 | 79 / 79 |

索引完成中位数变化均在 5% 内，target 物化持平；不变重扫在候选中较慢。用户目录列表 p95 的绝对差为约 3.5–5 ms，管理库列表 p95 回退更明显，应与实际媒体浏览接口区分。poster-worker 基准另将本地图片写入并行纳入测量，不把本地队列耗时并入索引时间。

poster-worker A/B 使用每个 movie 一张有效 1×1 PNG。候选父目录快照缓存降低重复 `read_dir`；baseline 使用旧本地图片后处理，候选用独立 outbox worker：

| 后端 / 文件数 | 首 poster ms B→C | scan job 完成 ms B→C | 本地 poster queue 完成 ms B→C | 目录 p95（写入期间 B→C） | 目录 p95（队列完成后 B→C） |
|---|---:|---:|---:|---:|---:|
| SQLite / 1k | 134 → 111 | 449 → 196 | 450 → 406 | 254 → 295 | 29 → 41 |
| PostgreSQL / 1k | 314 → 194 | 2,990 → 360 | 2,992 → 2,432 | 266 → 315 | 50 → 50 |
| SQLite / 10k | 1,028 → 347 | 9,338 → 1,000 | 9,365 → 5,580 | 259 → 301 | 29 → 37 |
| PostgreSQL / 10k | 1,719 → 603 | 59,866 → 2,314 | 59,875 → 37,345 | 261 → 326 | 55 → 58 |

候选 scan job 可先返回，poster worker 留在后台；10k fixture 首张 poster 提前约 0.7–1.1 s，完整本地 poster 队列比基线快约 30–38%。父目录缓存单独 A/B 将候选 10k poster queue 从 7.15→5.58 s（SQLite）、51.65→37.35 s（PostgreSQL）。扫描期间 50 并发目录 p95 为 0.295–0.326 s，poster 队列完成后的 p95 为 0.036–0.058 s；扫描期间 p95 相对旧流程增加约 40–65 ms。每 16 项让出 2/10 ms 的限速实验没有稳定改善 p95，且延长队列，未保留。当前 p95 仍低于 0.4 s，但 LUX-275/LUX-304 的 5% 回退门尚未满足；LUX-305/306 随后按有界 item image write 批次减少写事务并复测，结果见下文。结果只代表本机 ARM64 与 PostgreSQL 16.15，不能外推到 NAS/x86_64；阶段 23 尚未通过性能门。

### LUX-462 性能基准观测补全（2026-10-09）

基准 harness 增加 SQLx statement 延迟报告和连接池采样。LUX-270 分别输出 Manifest 首扫、postprocessing target 物化、目录页首次请求、热页请求，以及无变化重扫与并发前台请求的 SQL 汇总；每个规范化 SQLx statement summary 报告调用数、累计耗时、p50/p95/max。每个 SQL 延迟对象都带有 `phaseWindow`、`backgroundSqlMayBeIncluded` 和 `phaseWindowNote`，用于标明样本边界及并发后台 SQL 的归属。采集器使用进程级 SQLx listener，phase window 表示事件采集的时间区间，并不保证把每条语句归因到该请求；因此报告会将后台 SQL 可能混入显式标为 true。summary 仅使用 SQLx 提供的前四个 SQL token，折叠空白并统一大小写，不采集 bind 值或完整 SQL。SQLx `elapsed_secs` 不包含获取连接的等待；若某阶段没有延迟事件，结果标为 `unavailable`，不填零。PostgreSQL 锁监控 SELECT 从 SQL 延迟样本中排除；SQLite 锁监控语句若被 SQLx 记录，可能仍在样本内。无变化重扫的 SQL 样本与并发管理及目录请求重叠，报告会明确这一边界。

连接池在首扫、target 物化、管理 API、目录 API 首次请求和热页请求期间分别采样 `size`、`idle`、`in-use` 与饱和观察次数。采样间隔为 5 ms，读取 SQLx pool 的内存计数器；采样 task 会增加少量调度和计数读取开销，短于采样间隔的饱和也可能漏过，因此这不是 pool-acquire wait 的直接测量。目录页增加首次请求和同页 50 并发热页对照，并分别报告请求延迟、pool 压力和 SQLx 延迟摘要，便于观察首次缓存准备和后续 hydration/query 负载。单次首次目录请求只记录 `catalogListFirstRequestMs`，不从单个观察值计算请求 p50/p95；百分位数只对有多次请求的热页批次报告。

LUX-304 的本地图片队列只在 scan job 的每个 `scan_local_metadata_batches` 均为 `COMPLETED`、全部存在 `images_completed_at`、没有 pending/running/failed/cancelled 批次，且本地 poster 数达到 fixture 文件数时才算排空。结果分别记录 scan-active、scan 完成但 image batch 未排空期间，以及严格排空后的目录列表 p95。中间阶段只有在请求批次开始和结束时都仍有 pending/running batch，且没有 failed/cancelled batch 或已完成但缺少 `images_completed_at` 的 batch 时才报告数值；否则写为 unavailable 并说明状态原因。失败 batch 不会被称作 pending，可继续等待 worker 重试；已完成但缺少完成标记属于不一致状态，drain 轮询会立即报错。失败、取消或仍残留的 batch 不会被统计成完成。

验证（2026-10-09）：本机 `uname -m=arm64`，基准候选 revision `e487076b`，SQLite fixture 单轮运行。LUX-270 的 1k/100-directory fixture 通过并输出所有 SQL 延迟窗口与 pool 采样：Manifest DML 34 次、5 个批次；首个目录页请求 18 ms，同页 50 并发热请求 p50/p95 为 367/679 ms，并发前台请求 p95 为 692 ms。pool 采样均未观察到饱和；热页窗口最大 in-use 为 7/8。LUX-304 的 1k fixture scan p95 为 756 ms、drain 后 p95 为 726 ms；该轮 50 个 image-pending 请求开始时有 9 个 pending batch，结束时队列已 drain，因此该窗口正确报告 unavailable。10k/1,000-directory fixture 的 scan p95 为 780 ms、image-pending p95 为 755 ms、drain 后 p95 为 827 ms；pending 窗口首尾分别有 74 和 55 个待处理 batch，最终 88/88 个 batch 带 `images_completed_at` 完成且 10,000 张 poster 已登记。10k 首个条目可见 88 ms、首张 poster 可见 88 ms、scan job 完成 1,470 ms、本地图片队列完成 4,380 ms。

这些是单轮观测值，用于确认字段、SQL/pool 样本及严格 drain 条件工作；不是 A/B 或性能收益结论。全量 Rust build/Clippy/测试门禁仍待通过，LUX-304 历史 A/B 仍使用此前口径；当前结果只代表本机 ARM64 与 SQLite，不外推 FNOS、NAS/x86_64 或 PostgreSQL。

### LUX-306 批量本地图片写入 A/B：1k/10k SQLite/PostgreSQL

2026-09-29 在 Mac16,10 / 16 GiB / ARM64（`uname -m=arm64`），以 LUX-305 提交 `245d2f83` 为基线、LUX-306 worker 提交 `1bedd2e1` 为候选，对同一 1k/10k poster-worker fixture 在 SQLite 与 Docker PostgreSQL 16.15 上各交错运行三轮。每个 movie 有一张有效 1×1 PNG；每轮扫描期间及队列完成后各测 50 个并发 `GET /api/v1/libraries/{id}/items` 请求的 p95。表中为三轮中位数，单位 ms；首 poster 与 scan job 时间由 20 ms 轮询观察。候选每页最多准备并原子写入 16 个 movie，首个 movie 仍走单项快速路径。

| 后端 / 文件数 | 首 poster B→C | scan job 完成 B→C | local poster queue 完成 B→C | 活动扫描目录 p95 B→C | 队列完成后目录 p95 B→C |
|---|---:|---:|---:|---:|---:|
| SQLite / 1k | 105 → 109 | 134 → 128 | 417 → 321（快 23.0%） | 305 → 314（+3.0%） | 37 → 37 |
| SQLite / 10k | 382 → 282 | 1,084 → 883 | 5,648 → 4,685（快 17.1%） | 310 → 317（+2.3%） | 36 → 36 |
| PostgreSQL / 1k | 192 → 209 | 351 → 365 | 2,647 → 574（快 78.3%） | 321 → 320（−0.3%） | 57 → 53 |
| PostgreSQL / 10k | 654 → 658 | 2,211 → 2,212 | 36,667 → 13,003（快 64.5%） | 351 → 328（−6.6%） | 59 → 63（+4 ms） |

候选四组均在剩余 poster queue 完成前结束 scan job；本地队列中位数四组均缩短。首 poster 中位数在 SQLite/1k、PostgreSQL/1k、PostgreSQL/10k 分别变化 +4、+17、+4 ms，处于 20 ms 观察粒度内；SQLite/10k 提前 100 ms。活动扫描期间目录 p95 四组回退均低于 5% 门槛。10k PostgreSQL 队列完成后的 p95 中位数从 59 增至 63 ms（+4 ms、+6.8%），该差异单独保留记录，不计入活动扫描期间 p95 门槛。

一次额外的 SQLite/1k 基准候选运行触发了 poster 必须早于 scan 完成的断言；该次未留下时间样本。加入具体时间的断言错误信息后，诊断复跑通过，且正式三轮均通过。该失败被如实记录，不并入正式三轮统计。以上仅是本机 ARM64 与本地 PostgreSQL 容器 A/B，不外推到 NAS/x86_64，也不代表部署后或真实客户端验收；阶段 23 总体验收仍开放。

### LUX-323 完整性结果分片：结果 UPDATE 次数推导

2026-10-02 对扫描本地 metadata completeness 消费路径做静态计数，不作为耗时基准。每个 claim 批次最多 512 条 capability 结果，在线补缺候选最多 256 个 item，每个 FILL_MISSING 调度分片最多 100 个 item，因此最多分成 3 个事务。

存储事务对每条传入的完整性结果执行一次 `UPDATE item_metadata_completeness`。旧循环对每个调度分片重复传入整批结果，最坏为 512 × 3 = 1,536 次结果 UPDATE 尝试；新逻辑按 item ID 将结果分配至唯一事务，结果 UPDATE 尝试最多 512 次，减少最多 1,024 次（约 66.7%）。非 eligible 的 claim 结果随首个事务保存；已 READY/missing 但没有新 claim 的 eligible item 仍可通过空结果事务进入补缺调度。

该变化只计算完整性结果 UPDATE 尝试，不代表全部 SQL 数或端到端时延变化。仍保留最多 3 个事务、每个事务内结果与补缺意向原子提交的结构；没有新增 SQLite/PostgreSQL 性能样本。

### LUX-324 剧集合并分集读取计数

2026-10-02 在 SQLite 临时库中合并一部主剧集与一部源剧集，源剧集有 12 个没有同号目标季度的空季度。测试重置存储查询计数后，只计入完整合并事务发出的查询调用。

| 场景 | 合并查询调用 | 分集读取调用 |
|---|---:|---:|
| 原实现：每个源季度单独读分集 | 32 | 12 |
| 新实现：按主/源剧集根一次读取并按季度分组 | 21 | 1 |

该样本减少 11 条查询调用（约 34.4%）；优化点是移除季度数增长时的重复 SELECT。媒体源、用户状态、季度迁移等写操作保持逐条执行，因此本计数不代表端到端时延或大型真实剧集库性能。新读取使用 CTE 和固定 4 个参数，避免按季度数扩展 bind 列表；没有可用 PostgreSQL 实例做本任务行为复测。

### LUX-325 STRM 扫描读取上界

2026-10-02 将普通扫描与 manifest 回退读取改为最多读取 1 MiB + 1 字节，再以多出的一个字节判定超限；manifest root-relative 读取保留安全打开和同一上界。内容不会整文件无界读取，超限返回 `InvalidData`。这项验证的是输入读取边界，不是 I/O 调用计数或耗时基准，没有据此推断整体扫描性能提升。

### LUX-328 电影版本后缀重复文件查询计数

新增的可计数探测单测使用 `ADN-725-Alternate-Cut.mp4`，模拟同目录存在 `ADN-725.mp4`。当前候选算法对更长的 `ADN-725-Alternate` 和较短的 `ADN` 各探测 6 种扩展名，对命中的 `ADN-725` 探测 1 种扩展名，总计每次推断 13 次候选 metadata 查询。

完整常规重扫中，旧路径会在未变化检查、分组和实际扫描各推断一次，即代表性文件最多 39 次候选查询；现在未变化检查返回已推断后缀并沿分组传给扫描，降为 13 次，少 26 次（约 66.7%）。文件指纹已变化时，预检查会在版本推断前返回，旧路径的分组与扫描共 26 次，现在分组结果复用后为 13 次。reconciliation 分组与兼容性预检查也复用结果。

这些数字是对候选文件 metadata 查询尝试数的确定性计数，不是磁盘系统调用计数的外部采样，也不是耗时基准；没有据此推断本机、NAS 或网络文件系统上的扫描时延提升。

### LUX-329 人物 Manifest 身份恢复 SQL 计数

`restore_canonical_person` 的 storage 查询计数回归以 SQLite 为准。4 个身份时，逐项归属 SELECT、逐项身份 INSERT 和人物/序列/回读共发出 11 条查询；改为最多 100 个身份一批后，归属查询与身份 INSERT 各合并成一条，总计 5 条，减少 6 条（约 54.5%）。205 个身份时，旧路径为 413 条，新路径将归属检查与 INSERT 各拆成 3 个有界批次，并保留 3 条固定写/回读，总计 9 条，减少 404 条（约 97.8%）。

计数来自 `Database` storage 查询计数器，衡量的是 SQL 查询调用次数，不是数据库往返的网络采样或墙钟基准。实现每批最多 100 个身份，归属检查最多 200 个绑定值、批量 INSERT 最多 700 个绑定值；不据调用次数变化推断端到端耗时收益。

### LUX-330 人物索引重建任务启动同步

使用 4 个启用库和 1 个禁用库的 storage 测试，原路径在同步时查询一次启用库列表、逐库 upsert 四次、最后读取任务列表一次，共 6 条 SQL；批量 `INSERT ... SELECT ... ON CONFLICT` 后与最终列表共 2 条 SQL。SQLite UPDATE 触发器确认相同 schema 的未变化任务同步触发 0 次行 UPDATE；schema 版本变化时仍由同一批量 upsert 更新 4 行并重置字段。现有回归也覆盖了活动任务保持、超过 60 秒的 RUNNING 任务回收。

这些结果是 storage SQL 调用和 SQLite 行 UPDATE 触发计数，不是耗时基准；没有据此推断启动时长或 PostgreSQL/NAS 性能变化。

### LUX-331 人物清单恢复状态预读

使用 205 份有效人物清单，并预先写入与其 person ID、checksum 和 schema version 完全匹配的索引状态。逐清单校验路径发出 205 条 storage 查询；每 100 个 ID 批量读取状态后发出 3 条查询，未变化清单不再执行单项查询。状态表查询最多绑定 100 个 ID。该计数是 SQL 调用次数，不是数据库往返采样或墙钟基准；变化清单仍走原有单人物校验/事务恢复路径。

### LUX-332 Web 服务端 HLS chunk 大小

在 2026-10-02 的 macOS ARM64 开发机上，用同一工作树分别执行改动前和改动后的 `pnpm --dir web build`。Vite 将 `hls.js` 独立为 HLS 播放时按需加载的 chunk：

| 构建 | HLS chunk 原始大小 | gzip 大小 | 首页入口原始大小 | 首页入口 gzip |
|---|---:|---:|---:|---:|
| 完整 HLS.js | 594.13 kB | 185.60 kB | 113.77 kB | 30.20 kB |
| HLS.js light | 371.83 kB | 117.93 kB | 113.77 kB | 30.20 kB |

light build 的 HLS chunk 减少 222.30 kB（约 37.4%），gzip 减少 67.67 kB（约 36.5%）；首页入口大小未变，Vite 的 500 kB chunk 警告消失。Lux 自有 `SERVER_HLS` 输出为 fMP4/CMAF，FFmpeg 只映射一个视频和一个音频流，因此此路径不使用 light build 排除的 HLS 字幕、备用音轨和 DRM 功能。HLS.js 仍仅在浏览器没有原生 HLS 时动态加载。

本次没有记录真实浏览器的 LCP、MSE 首帧或播放启动时延，构建字节下降不代表这些时延已实测改善；数据也不外推为端到端播放性能或设备间差异。

### LUX-339 增量扫描路径队列 SQL 调用计数

2026-10-02 在 `uname -m=arm64` 的开发机上，以临时 SQLite 数据库向同一增量扫描任务加入 205 个唯一路径。改动前逐路径调用存储 upsert 与任务计数刷新，每条路径 2 次，共 410 次 SQL 查询调用。改动后按最多 100 条路径生成多行 upsert，并在每个批次后刷新一次任务计数：205 条路径分为 100、100、5 三批，共 3 条 upsert 和 3 条计数更新，即 6 次查询调用，减少 404 次（约 98.5%）。

候选测试额外提交一个重复路径，并验证最后的 `MODIFY` 类型胜出、已处理项的 `processed_at` 被清空、`total_count` 与 205 个唯一队列项一致。每条路径绑定 4 个值，100 条每批最多 400 个参数。该计数来自 SQLite 存储查询计数器，只衡量 SQL 调用数量，不是网络往返采样或墙钟基准；未据此声称具体耗时或 PostgreSQL/NAS 性能提升。

### LUX-340 扫描文件状态与 inode 写入计数

2026-10-02 在 SQLite 临时库中复现已有文件更新：原路径先执行状态 UPDATE，再执行媒体条目恢复查询，随后单独执行 inode UPDATE，共 3 条存储 SQL 调用；合并 inode 后仍执行状态 UPDATE 和恢复查询，共 2 条，减少 1 条（约 33.3%）。存储回归测试在实现前直接观察到 3 条，合并实现后验证为 2 条，并检查 size、modified_at、fingerprint、inode、scan generation 与 missing 状态。

未变化剧集回退路径的原 mark-seen 操作同样执行状态 UPDATE 与恢复查询，再单独更新 inode；现在 inode 随 mark-seen UPDATE 写入，计数从 3 降至 2。测试另验证 mark-seen 后 generation、inode 与 missing 状态。扫描器把当前 inode 一并传给电影、剧集和 sidecar 的已有条目更新。

SQL 调用计数不等于墙钟耗时或磁盘写入量，也没有 PostgreSQL/NAS 实测；未据此推断时延收益。

### LUX-341 轻量 Manifest 根初始化读取计数

2026-10-02 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 数据库中，以 4 个根初始化轻量 Manifest discovery session，其中 2 个根状态为 `COMPLETE` / `UNAVAILABLE`，另 2 个根需要 baseline 检查。旧流程先列出根 ID，再逐根读取状态，并只为活跃根查询 filesystem entry：`1 + 4 + 2 = 7` 次 SQL 调用。合并状态和 baseline 的查询后，真实 service 初始化只调用 1 次 SQL，下降约 85.7%；终态根继续跳过，两个活跃根的 baseline 标志保持正确。

该数值由 SQLite 查询计数器记录调用数，不是网络往返、数据库写入量或墙钟测量；没有 PostgreSQL 实例，也不据此推断时延或 NAS/x86_64 性能。

### LUX-342 本地元数据回填根注册调用计数

2026-10-02 在 `uname -m=arm64` 的开发机临时 SQLite 库中，以 4 个 library root 注册本地元数据回填队列。旧流程先读取根 ID，再逐根执行幂等注册，共 5 次 SQL 调用，4 次逐根 INSERT 各自获取一次写锁；合并为单条 `INSERT ... SELECT ... ON CONFLICT DO NOTHING` 后共 1 次 SQL 调用，减少 4 次（80%），整批只获取一次写锁。测试也验证空根列表与已注册根仍各用 1 次调用，且 affected-row 数分别为 0；首次插入 4 个队列行并返回 4，重复注册返回 0。

SQLite 查询计数只衡量 SQL 调用数量，不是数据库写入量或墙钟基准；没有 PostgreSQL 实例，也不据此推断 PostgreSQL、NAS 或 x86_64 性能。

### LUX-343 Webhook 投递队列 SQL 调用计数

2026-10-02 使用 205 个启用 destination 的固定 fixture。原实现写入 1 条 event 后逐目标执行 205 条 delivery INSERT，共 206 次 SQL 调用；新实现每批最多 100 个目标，发出 1 条 event INSERT 与 3 条多行 delivery INSERT，共 4 次，减少 202 次（约 98.1%）。SQLite 与 PostgreSQL 17 均验证 205 条投递行、重复 dedupe key 不增加投递；SQLite 另验证空目标只入事件，以及第二批失败时 event 和前一批 delivery 一起回滚。

每行绑定 3 个值，批次上限 100 使单条语句最多 300 个绑定参数，低于 SQLite 保守限制。计数来自 storage 查询计数器，只衡量 SQL 语句调用数，不是落盘行数或墙钟；未据此推断 NAS 或生产负载时延。

### LUX-346 章节检测任务条目写入调用数

2026-10-02 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中写入 205 个章节检测条目。旧实现逐项 INSERT，共 205 次存储 SQL 调用；新实现每批最多 100 条，多行 INSERT 共 3 次，减少 202 次（约 98.5%）。回归同时验证 205 行和 PENDING 状态、source/input fingerprint、context 标记；空输入为 0 次调用，第三批重复 source 触发约束错误后整页回滚。

每行绑定 7 个参数，批次上限 100 使每条语句最多 700 个绑定值。该多行 VALUES 语法由 SQLite 验证，且不依赖数据库专有扩展；没有 PostgreSQL 实例复测。计数来自 SQLite storage 查询计数器，只表示 SQL 调用数量，不是事务数、磁盘写入量、墙钟基准或端到端任务时延；不据此推断 PostgreSQL、NAS 或 x86_64 性能。

### LUX-347 媒体探测轨道替换调用数

2026-10-02 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中，为一个媒体源替换 205 条探测轨道。旧实现执行 source UPDATE、旧轨道 DELETE 和 205 次逐条 INSERT，共 207 次存储 SQL 调用；新实现每批最多 75 条轨道，发出 source UPDATE、DELETE 和 3 条多行 INSERT，共 5 次，减少 202 次（约 97.6%）。回归验证了 205 条轨道的顺序、字段、字幕外部路径和 disposition 标记，空轨道只执行 UPDATE 与 DELETE，第三批重复索引触发约束错误时 source 与旧轨道保持不变。

每行绑定 12 个值，批次上限 75 使每条语句最多 900 个绑定值。计数来自 SQLite storage 查询计数器，只衡量 SQL 调用数，不是事务数、磁盘写入量、墙钟基准或端到端探测时延；没有 PostgreSQL、NAS 或 x86_64 实测，不据此推断部署收益。

### LUX-348 章节检测插件模式重复读取

2026-10-02 检查章节检测任务的固定批大小：任务启动时已读取一次 `remote_lookup`，旧流程在每次 `process_season` 批次中再次读取 plugin catalog。对单季 10,000 集 fixture，按本地检测每批 64 集计算最多 157 次批次级重复读取，在线 lookup 每批 24 集最多 417 次；改动后两种模式都只保留任务级 1 次读取。该数字是基于批大小的静态上界，不是 catalog 锁竞争、墙钟时延或数据库性能测量。

### LUX-349 Emby 手动合集成员变更调用数

2026-10-02 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中，为一个合集处理 205 个成员 ID。旧新增路径先读取合集、分别读取 collection ID 和最大排序值，再逐项执行 205 次 `INSERT ... SELECT`，共 208 次存储 SQL 调用；新路径将两次预读合并，并按 100/100/5 分三批插入，共 5 次，减少 203 次（约 97.6%）。旧删除路径读取合集后逐项执行 205 次 DELETE，共 206 次；新路径按最多 500 个 ID 一批执行 1 条 DELETE，共 2 次，减少 204 次（约 99.0%）。回归验证了 205 个成员的数量和顺序、重复 ID 冲突幂等以及删除后为空。

每条新增批语句最多绑定 202 个值，删除批最多绑定 501 个值，低于项目 SQLite 保守上限。计数来自 storage 查询计数器，只衡量 SQL 调用数，不是墙钟、磁盘写入或 PostgreSQL/NAS 生产收益。

### LUX-350 本地元数据完整性领取调用数

2026-10-02 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中提交 205 条唯一 item/capability 检查。旧实现每条执行一次 upsert 和一次 claim UPDATE，共 410 次 storage SQL 调用；新实现按 100/100/5 分三批执行多行 upsert 和批量 claim，共 6 次，减少 404 次（约 98.5%）。回归验证 205 个原始索引全部返回、最终状态均为 `RUNNING`，并复用了既有并发、版本替换、READY/RUNNING、失败重试和输入校验覆盖。

每条批量语句最多绑定 300 个值，低于项目 SQLite 保守上限。计数来自 storage 查询计数器，只衡量 SQL 调用数，不是事务数、墙钟或磁盘写入；没有据此推断 PostgreSQL、NAS 或生产负载收益。

### LUX-351 本地元数据完整性结果更新调用数

2026-10-02 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中提交 205 条 RUNNING 完整性结果，关闭自动补缺以隔离结果写入。旧实现逐条 UPDATE 并先执行 1 次库存在校验，共 206 次 storage SQL 调用；新实现按 100/100/5 分三批更新并保留同一库校验，共 4 次，减少 202 次（约 98.1%）。回归验证 205 条 READY、`updated_count` 和交错缺失标记，批量 `RETURNING` 只计入实际更新行。

每条批量更新最多绑定 501 个值。计数来自 storage 查询计数器，只衡量 SQL 调用数，不是事务数、墙钟或磁盘写入；没有据此推断 PostgreSQL、NAS 或生产负载收益。

### LUX-352 用户媒体库排序替换调用数

2026-10-02 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中替换 205 个用户媒体库排序。旧实现执行 1 条 DELETE 和 205 次逐条 INSERT，共 206 次 storage SQL 调用；新实现执行 1 条 DELETE 和 100/100/5 三条多行 INSERT，共 4 次，减少 202 次（约 98.1%）。回归验证了 205 个位置顺序、空排序和重复库 ID 触发约束错误时整笔事务回滚。

每条批量 INSERT 最多绑定 300 个值。计数来自 storage 查询计数器，只衡量 SQL 调用数，不是事务数、墙钟或磁盘写入；没有据此推断 PostgreSQL、NAS 或生产负载收益。

### LUX-353 计划媒体库关联写入调用数

2026-10-02 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中向一个计划写入 205 个媒体库关联。旧路径的关联写入执行 205 次逐库 INSERT；连同批量 DELETE 和配置 UPDATE 共 207 次 storage SQL 调用。新路径按 100/100/5 三批多行 INSERT，共 5 次，减少 202 次（约 97.6%）。回归验证关联数量、计划镜像读取、调度和删除计划路径。

每条批量 INSERT 最多绑定 200 个值。计数来自 storage 查询计数器，只衡量 SQL 调用数，不是事务数、墙钟或磁盘写入；没有据此推断 PostgreSQL、NAS 或生产负载收益。

### LUX-354 计划媒体库任务配置读取调用数

2026-10-02 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中校验 205 个媒体库任务配置。旧实现按媒体库逐个 SELECT，共 205 次 storage SQL 调用；新实现按 100/100/5 分三批读取，共 3 次，减少 202 次（约 98.5%）。回归验证批量结果按输入顺序校验，缺失配置、重复 ID、source/plugin 不匹配和空输入仍保持原错误语义。

每批最多绑定 101 个值（任务类型加 100 个媒体库 ID）。计数来自 SQLite storage 查询计数器，只衡量 SQL 调用数，不是网络往返、墙钟或数据库写入量；没有据此推断 PostgreSQL、NAS 或生产负载收益。

### LUX-355 计划移出媒体库关联迁移调用数

2026-10-02 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中更新一个包含 705 个媒体库的自定义计划，并将 704 个媒体库移回默认计划。旧实现对每个移出库分别 DELETE 和 INSERT，完整更新路径共 1,420 次 storage SQL 调用；新实现按 500/204 两批使用 `INSERT ... SELECT` 和 DELETE，共 16 次，减少 1,404 次（约 98.9%）。回归验证自定义计划保留 1 个关联、默认计划接收 704 个关联及对应任务配置镜像。

每批最多绑定 502 个值（默认计划、源计划和 500 个媒体库 ID）。计数来自 SQLite storage 查询计数器，只衡量 SQL 调用数，不是网络往返、墙钟或数据库写入量；没有据此推断 PostgreSQL、NAS 或生产负载收益。

### LUX-356 手动媒体合并调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中合并 100 个同库电影根条目。旧实现逐条读取根记录，并对每个源条目分别执行媒体源迁移、用户状态合并/清理和根条目标记，共 596 次存储 SQL 调用；新实现按最多 100 个 ID 一批读取根记录，并将电影合并的三类写入改为有界批量 SQL，共 7 次，减少 589 次（约 98.8%）。请求返回的 99 个合并 ID 顺序保持一致。

7 次调用由根记录读取、主条目默认源检查、媒体源批量迁移、默认源归一化、用户状态批量 upsert、源状态批量清理和根条目批量标记组成。每批最多绑定 100 个根 ID，使用 SQLite/PostgreSQL 通用参数化 SQL；剧集层级合并仍保留原有逐层顺序写入，并跳过只供电影路径使用的主条目默认源读取，使现有分集批量读取回归由 20 次降为 19 次。计数来自 SQLite storage 查询计数器，只衡量 SQL 调用数，不是网络往返、墙钟、磁盘写入或 PostgreSQL/NAS 生产收益。

### LUX-382 媒体源批量删除调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中删除同一电影条目的两个媒体源。旧路径每个源分别执行存在性 SELECT、DELETE 和条目层级 UPDATE，共 6 次 storage SQL 调用；新路径按有界源 ID 集合执行一次校验、一次 DELETE 和一次层级 UPDATE，共 3 次，减少 3 次（50%）。回归同时验证了源/item 不匹配与缺失源不会部分写入，以及剧集的分集、季度和系列层级最终全部移除。

应用层每批最多提交 250 个源，使最坏的 item、parent、series 三组层级 ID 不超过 750 个绑定值；剧集路径在同一事务中按层级顺序更新，避免父级条件判断读取到未提交的子级状态。计数来自 SQLite storage 查询计数器，只衡量 SQL 调用数，不是网络往返、墙钟、磁盘写入或 PostgreSQL/NAS 生产收益。

### LUX-383 计划任务媒体库配置读取调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中读取两个媒体库的 `RECONCILIATION_SCAN` 任务配置。旧的计划派发路径按媒体库分别调用配置查询；新路径对 owner ID 去重后一次批量查询，固定配置读取为 1 次。运行任务仍按媒体库独立创建，未注册 owner 只影响自身并继续处理其他 owner。

批量读取每次最多绑定 500 个 owner ID，使用 SQLite/PostgreSQL 通用参数化 SQL；计数只衡量 storage SQL 调用，不是网络往返、计划任务墙钟、PostgreSQL/NAS 或生产收益。

### LUX-384 无计划任务配置重复读取

无计划任务分页已经返回完整 `scheduled_task_configs` 行，旧路径随后每个任务再次按 owner 执行同一配置 SELECT；新路径直接复用分页结果进入执行分发，移除每个无计划任务的一次重复读取。新增回归验证无计划媒体库扫描仍创建运行任务；该结论只来自代码调用边界和行为回归，不推断墙钟、PostgreSQL、NAS 或生产收益。

### LUX-385 服务器设置批量写入调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中保存一组包含播放阈值、媒体策略、管理员库排序和登录背景来源的服务器设置。旧实现对固定五个键逐条 UPSERT，共 5 次 storage SQL 调用；新实现用一条固定五行多值 UPSERT，共 1 次，减少 4 次（80%）。回归同时校验五个键的最终值和冲突更新语义。

该优化只合并同一事务内的固定设置写入，仍保留 `updated_at` 更新和 SQLite/PostgreSQL 参数绑定；计数来自 storage 查询计数器，只衡量 SQL 调用数，不是管理接口墙钟、锁等待、PostgreSQL、NAS 或生产收益。

### LUX-386 插件卸载后的媒体库刮削器重排调用数

2026-10-05 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备两个各含被卸载插件和备用/补充刮削器的媒体库。旧实现对每库分别读取、删除、逐项重插、读取主刮削器并更新库，共 15 次 storage SQL 调用；当前合并后的实现按最多 100 个媒体库批量读取和删除，按有界批次重插并用一条 `CASE` 更新主刮削器，共 6 次，减少 9 次（60%）。回归验证位置、角色、空刮削器、章节源清理和安装记录删除。

该计数只覆盖卸载事务内的刮削器重排 SQL；插件文件删除、目录扫描、任务同步、事务墙钟、锁等待、PostgreSQL、NAS 和生产收益不在数值范围内。

### LUX-387 Web 播放会话批量回收调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备三个过期和三个不活跃的 Web HLS 会话。旧实现每类先 SELECT 再逐会话条件 UPDATE，共 4 次 storage SQL 调用；新实现每类先 SELECT，再用一条带 ID 集合和 `RETURNING` 的条件 UPDATE，共 2 次，减少 2 次（50%）。多会话回归验证两类清理的返回数量和调用数，既有单会话回归验证停止状态。代码保留过期、计划和心跳条件，并按 `RETURNING` 返回实际成功集合；本次没有增加竞争条件的复现测试。

该计数只覆盖会话状态回收 SQL，不包含 HLS 临时目录删除、播放事件、锁等待、PostgreSQL、NAS 或生产收益。

### LUX-388 剧集合并额外分集重挂载调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中合并一个没有目标季度的源剧集，源季度包含 20 个分集。旧实现对季度和每个分集逐条更新，完整合并共 28 次 storage SQL 调用；新实现按最多 100 个分集 ID批量更新，共 9 次，减少 19 次（约 67.9%）。另有回归覆盖存在目标季度但集号未匹配时的 `parent_id + series_id` 重挂载。

该计数只覆盖固定层级重挂载 fixture；匹配分集的媒体源迁移、用户状态合并、合并标记、事务墙钟、PostgreSQL、NAS 和生产收益不在数值范围内。

### LUX-389 剧集合并匹配分集映射调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备 20 个唯一集号匹配的源/目标分集。旧实现每个匹配分集分别执行媒体源迁移与默认源归一化、用户状态写入与删除、合并标记，共 110 次 storage SQL 调用；新实现用映射 CTE 批量处理，共 15 次，减少 95 次（约 86.4%）。重复目标集号的异常形状回退逐项路径并保留版本递增语义。

该计数只覆盖固定匹配分集合并 fixture；季度读取、未匹配分集、事务墙钟、锁等待、PostgreSQL、NAS 和生产收益不在数值范围内。

### LUX-390 季度与剧集已看状态同步调用数

2026-10-04 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备一个含季度和剧集两个父级、两个可播放分集的 fixture。旧实现先读取父级，再对每个父级分别聚合分集状态并 UPSERT，共 5 次 storage SQL 调用；新实现用一条有界父级状态查询和一条多行 UPSERT，共 3 次，减少 2 次（约 40%）。回归验证全看、取消已看、重复同步、播放次数、版本和父级无可播放分集行为。

该计数只覆盖一次分集播放状态同步的 SQL 调用；播放回调其他查询、事务墙钟、锁等待、PostgreSQL、NAS 和生产收益不在数值范围内。

### LUX-391 媒体库刮削器配置写入调用数

2026-10-05 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备 5 个有序 scraper 配置。旧创建/编辑循环按源码每项执行一条 `library_scrapers` INSERT，5 项对应 5 条；新 helper 实测为一条多行 INSERT，相比旧循环减少 4 条（80%）。创建和编辑路径共用同一有界 helper，最多 100 行/400 个绑定参数一批。应用层仍最多 16 个 scraper；存储边界额外以 205 行验证分三批、空输入零 SQL 及后续批次失败回滚。

该计数只覆盖 scraper 行写入，不包含媒体库设置、计划任务同步、事务墙钟、PostgreSQL、NAS 或生产收益。

### LUX-357 STRM 探测任务媒体库预读调用数

2026-10-02 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中为 64 个媒体库创建 STRM 探测任务。旧实现对每个媒体库读取完整库、刮削器和 STRM 来源计数，再写入并回读任务，共 322 次 storage SQL 调用；新实现按 64 个 ID 一批聚合读取库存在性与 STRM 计数，再写入并回读任务，共 131 次，减少 191 次（约 59.3%）。回归验证 64 个任务、顺序和零来源计数。

聚合读取每批最多绑定 100 个媒体库 ID。计数来自 SQLite storage 查询计数器，只衡量 SQL 调用数，不是网络往返、墙钟或数据库写入量；没有据此推断 PostgreSQL、NAS 或生产负载收益。

### LUX-358 扫描本地元数据完整性预检读取调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备 205 个 active movie item。旧 scanner 预检对每个 item 分别读取完整元数据和媒体库归属，共 410 次 storage SQL 调用；新路径按最多 500 个 ID 联合读取元数据与 `library_id`，共 1 次，减少 409 次（约 99.8%）。返回的 205 个 item 和库归属均保持一致。

该计数只覆盖完整性预检的两类重复读取；图片索引、NFO 投影、人物关系、attempt 状态、claim、结果提交和在线补缺仍按各自现有边界执行。计数来自 SQLite storage 查询计数器，不代表完整 worker 墙钟、数据库写入量或 PostgreSQL/NAS 生产收益。

### LUX-359 元数据任务创建前校验读取调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中创建包含 100 个有效 movie item 的元数据任务。旧实现逐 item 读取类型和完整元数据，校验阶段共 200 次 SQL；连同任务写入与回读的完整创建路径共 205 次。新实现按最多 500 个 ID 一批读取元数据，校验阶段 1 次、完整路径 6 次，减少 199 次完整路径调用（约 97.1%）。

缺失 item 和 VIDEO 类型仍按输入顺序返回原错误，去重和任务上限未改变。计数来自 SQLite storage 查询计数器，只衡量 SQL 调用数，不代表元数据 worker 墙钟、网络请求、PostgreSQL 或 NAS 生产收益。

### LUX-360 章节检测 marker 替换写入调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中为一个媒体源替换同一 provider 的 3 个 marker，并启用 source fingerprint 校验。旧实现执行 fingerprint SELECT、旧 marker DELETE 和 3 次逐条 INSERT，共 5 次 storage SQL 调用；新实现将 3 行合并为 1 条多值 INSERT，共 3 次，减少 2 次（约 40%）。

空 marker、fingerprint 不匹配和其他 provider 的 marker 仍沿用原有事务语义。计数来自 SQLite storage 查询计数器，只衡量 SQL 调用数，不代表章节检测端到端墙钟、PostgreSQL 或 NAS 生产收益。

### LUX-361 本地元数据完整性计划依赖预读调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备 205 个 active movie item，分别读取媒体策略、图片索引和 metadata attempt 状态。旧路径对每个 item 各执行一次查询，共 615 次；新路径按最多 500 个 ID 批量读取三类依赖，共 3 次，减少 612 次（约 99.5%）。

批量计划仍逐 item 执行本地图片文件存在性、NFO 投影和人物关系文件检查；这些文件读取、后续刮削器资格查询和完整 worker 墙钟不在本次计数范围。计数来自 SQLite storage 查询计数器，只衡量 SQL 调用数量，不代表数据库写入量、PostgreSQL、NAS 或生产收益。

### LUX-362 刮削器配置预读调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备 205 个使用同一有序 `library_scrapers` 配置的 movie item。旧 resolver 配置读取逐 item 发出 205 次查询；新 storage 入口按最多 500 个 ID 一批读取，共 1 次，减少 204 次（约 99.5%）。另以无有序配置的 item 验证 legacy `libraries.scraper_id` fallback 仍只在单 item resolver 中触发。

该计数只覆盖配置读取，不包含后续插件客户端解析、RPC、缓存命中或 scanner 墙钟；SQLite 查询调用数不代表 PostgreSQL、NAS 或生产收益。

### LUX-363 本地完整性补缺资格批量读取

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的 205 item fixture 上，scanner 只收集有 requestable capability 的 item，并通过 resolver 一次批量加载刮削器配置；配置 SQL 保持 1 次，随后逐 item 使用既有客户端缓存判断可用性。

该记录只覆盖配置读取调用数，不把插件客户端解析、RPC、缓存命中和 scanner 墙钟混入 SQL 结果，也不推断 PostgreSQL、NAS 或生产收益。

### LUX-364 本地完整性图片写回源上下文预读

2026-10-03 在 `uname -m=arm64` 的临时 SQLite 库中准备 205 个 active movie item。旧路径在图片本地检查中对每个 item 分别读取媒体类型和可写回源路径，共 410 次 storage SQL 调用；新路径按最多 500 个 ID 批量读取写回上下文，共 1 次，减少 409 次（约 99.8%）。回归同时覆盖电影直接源和剧集首集源的选择。

该计数只覆盖写回上下文的数据库预读；本地图片文件检查、NFO projection、人物关系文件、attempt 状态、完整性结果提交和在线补缺不在本次数值范围内。计数来自 SQLite storage 查询计数器，不代表数据库往返、墙钟、PostgreSQL、NAS 或生产收益。

### LUX-365 插件安装状态批量读取

2026-10-03 在 `uname -m=arm64` 的临时 SQLite 库中准备 205 个插件 ID，其中两个有安装状态。旧路径逐插件读取 `installed_plugins`，共 205 次 storage SQL 调用；新路径按最多 500 个 ID 批量读取，共 1 次，减少 204 次（约 99.5%）。回归验证未安装、已禁用和已启用三态映射。

该计数只覆盖安装状态查询；动态插件视图的配置文件解析、运行状态读取、插件 RPC 和管理接口墙钟不在本次数值范围内。计数来自 SQLite storage 查询计数器，不代表数据库往返、墙钟、PostgreSQL、NAS 或生产收益。

### LUX-366 章节检测计划同步重复读取上界

同步逻辑原来对每个已启用章节插件重新读取一次全部媒体库，并在每个选中库再次读取同一插件设置。改动后，媒体库列表在本次同步中最多读取 1 次，每个有效插件的设置只解析 1 次并复用到其选中库；安装状态复用 LUX-365 的批量读取。该结果是由循环边界和调用位置得到的静态调用上界，未测量墙钟、配置文件解析耗时、PostgreSQL、NAS 或生产收益。

### LUX-367 图片路径冲突修复的候选读取上界

图片冲突修复原来对每个 item 的每个候选 `-thumbnail[-N]` 路径执行一次 `item_images` 占用查询，候选上界为 1,000 次。改动后按冲突 item ID 批量预读图片索引（每批最多 500 个 ID），候选筛选不再访问数据库；实际准备更新前保留至多一次占用复核。该结果是静态 SQL 调用上界，不是墙钟或磁盘测量，也不外推 PostgreSQL、NAS 或生产收益。

### LUX-368 插件服务循环安装状态读取上界

章节源列表、Manifest scheduled task 同步和 IP location provider 选择原来分别在插件循环中逐个读取 `installed_plugins`；改动后每个服务调用先收集候选 ID，再按最多 500 个 ID 批量读取并复用状态。该结果是静态循环调用上界，未测量动态配置文件解析、运行状态、墙钟、PostgreSQL、NAS 或生产收益。

### LUX-369 插件视图媒体库选项读取调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备两个带 `media-libraries` 配置字段的本地插件，并请求已安装插件列表。旧路径除安装状态批量查询外，每个插件分别读取媒体库和 scraper 关联，共 5 次 storage SQL 调用；新路径在列表请求中懒加载一次媒体库快照并复用，共 3 次，减少 2 次（40%）。回归验证两个插件的动态视图仍返回媒体库选项。

该快照只在列表请求内复用；单插件配置接口继续独立读取，章节插件仍过滤电影库。计数来自 SQLite storage 查询计数器，只衡量 SQL 调用数，不是墙钟、插件 RPC、PostgreSQL、NAS 或生产收益。

### LUX-370 Manifest 任务禁用镜像调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备同一插件的两个 Manifest task，并更新其配置/计划镜像。旧路径对每个 task 分别开启事务，分别 UPDATE 两张表，共 4 次 storage SQL 调用；新路径在一个事务中按 task 类型 `IN` 更新两张表，共 2 次，减少 2 次（50%）。回归验证两个 task 的配置表和计划表均被停用。

该优化只合并禁用镜像更新；后续 owner 注册仍按现有字段和顺序执行。计数来自 SQLite storage 查询计数器，只衡量 SQL 调用数，不是事务墙钟、插件 RPC、PostgreSQL、NAS 或生产收益。

### LUX-371 NFO probe 写回上下文读取边界

2026-10-03 检查电影 probe NFO 写回的 SQL 路径。旧路径先读取 item kind、写回 source，再由通用 target 解析重复读取 kind 和 source；新路径一次读取 `StoredMediaWritebackContext`，并复用其中的电影 source 完成 target 路径检查。probe target 选择阶段由 4 次重复类型/源读取收敛为 1 次上下文查询；写回后的通用 auxiliary、fingerprint 和 invalidation SQL 不计入该边界。

该记录是由固定调用路径得到的 SQL 边界，不是墙钟或磁盘基准；NFO writer、series metadata 和 metadata 回归通过，但未据此推断 PostgreSQL、NAS 或生产收益。

### LUX-372 本地 NFO enrichment 元数据读取边界

2026-10-03 检查单条 NFO enrichment 的媒体元数据读取路径。旧流程在身份冲突校验和最终写回之间分别执行两次完整 `find_media_item_metadata` 查询；新流程复用第一次结果，固定路径由 2 次完整读取降为 1 次。provider ID、NFO cache 和人物关系写入不修改媒体元数据列，因此没有引入额外刷新查询。

该记录是 SQL 调用边界，不是墙钟或锁竞争基准；metadata、series metadata 和 NFO writer 回归通过，未据此推断 PostgreSQL、NAS 或生产收益。

### LUX-373 STRM resolver 安装状态读取调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备两个已安装的 STRM resolver 插件，并检查 resolver 可用性。旧实现对每个 resolver 分别读取 `installed_plugins`，共 2 次 storage SQL 调用；新实现收集 resolver ID 后执行 1 次有界批量查询，减少 1 次（50%）。动态插件视图、可用性过滤和 resolver 顺序保持不变。

计数来自 SQLite storage 查询计数器，只衡量安装状态读取的 SQL 调用数，不代表插件配置文件解析、resolver RPC 墙钟、PostgreSQL、NAS 或生产收益。

### LUX-374 旧章节插件库选择迁移调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备两个可分配剧集库、一个电影库、一个已有章节源库和一个重复 ID，并运行旧章节插件的 `libraryIds` 迁移。旧实现对每个配置 ID执行完整 `find_library`，再对两个可分配库执行 `update_library_settings`，共 10 次 storage SQL 调用；新实现先读取一次插件状态，再用一个有界条件 UPDATE 为两个未分配章节源的库写入，共 2 次，减少 8 次（80%）。

条件更新只命中存在、非电影且 `chapter_source_id IS NULL` 的库；计数来自 SQLite storage 查询计数器，不代表迁移墙钟、PostgreSQL、NAS 或生产收益。

### LUX-375 元数据写回策略读取调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备一个启用媒体库和一条 active movie item，并检查写回策略。旧实现先读 item 的库 ID，再读完整库（含 scraper 关联）和全局策略，共 4 次 storage SQL 调用；新实现使用已有 item/library JOIN 一次取得本地及全局策略，共 1 次，减少 3 次（75%）。

计数来自 SQLite storage 查询计数器，只衡量策略判断 SQL 调用数，不代表 NFO/图片写回墙钟、PostgreSQL、NAS 或生产收益。

### LUX-376 Manifest 媒体库 owner 注册调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中注册 205 个唯一 `LIBRARY` task owner，并额外传入 1 个重复 owner。旧实现逐 owner upsert，共 206 次 storage SQL 调用；新实现先去重，再按 100/100/5 三批多行 upsert，共 3 次，减少 203 次（约 98.5%）。

计数来自 SQLite storage 查询计数器，只衡量 owner 配置写入 SQL 调用数，不代表 Manifest 同步墙钟、GLOBAL 计划镜像、PostgreSQL、NAS 或生产收益。

### LUX-377 弹幕多媒体库任务创建调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中配置两个媒体库并创建弹幕匹配任务。旧实现每个库重复读取弹幕配置、媒体库选项、插件状态和动态可用性，共 26 次 storage SQL 调用；新实现复用一次设置读取和一次可用性检查，共 19 次，减少 7 次（约 26.9%）。每库任务仍独立写入并回读。

计数来自 SQLite storage 查询计数器，只衡量任务创建路径的 SQL 调用数，不代表配置文件解析、插件 RPC、PostgreSQL、NAS 或生产收益。

### LUX-378 Manifest 同步媒体库选项调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中安装两个带媒体库选项和 GLOBAL task 的插件，再同步 Manifest task。旧实现每插件各读一次媒体库和 scraper 关联，共 15 次 storage SQL 调用；新实现复用一次懒加载快照，共 13 次，减少 2 次（约 13.3%）。GLOBAL 计划和 task 写入次数未改变。

计数来自 SQLite storage 查询计数器，只衡量同步路径 SQL 调用数，不代表配置文件解析、插件 RPC、PostgreSQL、NAS 或生产收益。

### LUX-379 缩略图 scraper 重试首轮读取调用数

2026-10-03 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备一个带 scraper 的本地电影条目并检查 scraper-first 重试状态。旧实现先读取本地缩略图源和全局策略，再在图片缺失判断中重复读取本地源并读取图片索引，共 4 次 storage SQL；新实现复用同一源读取并在状态判断中读取一次全局策略和图片索引，共 3 次，减少 1 次（25%）。

元数据刷新完成后的最终图片检查仍独立重新读取最新源和图片索引，避免缓存刷新前状态。计数来自 SQLite storage 查询计数器，只衡量首轮状态读取，不代表刷新墙钟、文件检查、PostgreSQL、NAS 或生产收益。

### LUX-380 弹幕任务取消状态读取调用数

当前弹幕 worker 对每页最多 100 个待处理条目逐条查询 `cancel_requested`，并在页面结束再查询一次；无取消请求时固定边界为 101 次状态读取。改为首项和每 8 个条目查询一次，100 条页面为 13 次间隔检查加 1 次最终检查，共 14 次，减少 87 次（约 86.1%）。

该优化保留最多 8 个条目的取消响应边界；统计只覆盖取消状态 SQL，不包含待处理列表、claim、worker 写回或插件 RPC，也不推断墙钟、PostgreSQL、NAS 或生产收益。

### LUX-381 STRM 缩略图图片登记调用数

STRM 截图成功后原实现对同一文件分别 upsert `POSTER`、`THUMB`，再单独更新 `poster_fallback_required`，固定为 3 次写入和 3 个事务。改用已有有界图片批量写入后，两条图片记录和 fallback 清除在一个批量图片事务中完成，固定为 2 次 SQL 写入和 1 个事务，减少 1 次 SQL（约 33.3%）并减少 2 个短事务。

该记录只覆盖图片登记与 fallback 清除，不包含图片文件写入、STRM 插件 RPC、媒体信息写回或任务进度更新；未据此推断墙钟、PostgreSQL、NAS 或生产收益。

### LUX-383 Web HLS 会话清理调用数

2026-10-05 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备 130 个过期会话和 130 个无心跳 `SERVER_HLS` 会话。旧路径先读取最多 128 个候选，再逐会话执行条件 UPDATE，固定为 129 次 storage SQL 调用；新路径保留候选上限，使用一次参数化 `UPDATE ... RETURNING id` 批量停止，固定为 2 次，减少 127 次（约 98.4%）。回归验证两条路径均只停止前 128 个，剩余 2 个仍为 active，并按候选顺序返回实际停止会话。

该计数只覆盖会话状态清理 SQL；HLS 临时目录删除、调度间隔、锁等待、PostgreSQL、NAS 和生产收益不在数值范围内。计数来自 SQLite storage 查询计数器，不代表数据库往返或端到端墙钟。

### LUX-384 插件卸载媒体库刮削器重建调用数

2026-10-05 在 ARM64 开发机（`uname -m=arm64`）的临时 SQLite 库中准备 205 个媒体库，每库包含待卸载插件的 PRIMARY、SUPPLEMENT 和 BACKUP 三条配置。旧路径先逐库读取/删除/重插配置，再逐库读取主刮削器并更新媒体库，共 1,234 次 storage SQL 调用；新路径读取一次受影响库快照，按有界批次删除、插入和更新，共 12 次，减少 1,222 次（约 99.0%）。回归验证位置重排、PRIMARY/BACKUP 角色和 legacy `scraper_id` 保持正确。

该计数只覆盖数据库事务中的插件配置重建；插件文件删除、章节源同步、配置文件清理、PostgreSQL、NAS 和生产收益不在数值范围内。计数来自 SQLite storage 查询计数器，不代表端到端墙钟。

### LUX-383 普通本地图片登记事务边界

普通电影、剧集、季度和分集图片索引现在统一复用有界 `ItemImageBatchInsert` 写入。图片 upsert 与 `poster_fallback_required` 清理在同一 metadata 写事务中完成；故障注入验证 fallback 更新失败时不会留下部分 `item_images`。该记录只说明事务原子性和 SQL 边界，不代表 FNOS/PostgreSQL/NAS 墙钟或 CPU 收益。

### LUX-384 FILL_MISSING 创建去重边界

通用 `create_fill_missing_job` 现在在同库事务内复用活动条目去重和 queued job 合并；重复条目不会再创建新的 `metadata_reidentify_jobs` 行，新条目只追加到已有 queued job 的容量内。固定 storage 回归验证重复调用最终保留一个 job 和两条 job item；该记录只说明任务创建边界，不代表 FNOS/PostgreSQL/NAS CPU 或墙钟收益。

### LUX-385 取消 metadata job 的残留 item 清理

取消 job 时，仍为 `PENDING/RUNNING` 的 item 统一进入 `FAILED/JOB_CANCELLED`，历史取消 job 由 migration 进行同样的幂等收尾；管理员 retry 时恢复为 `PENDING`。该记录说明队列状态收敛和历史数据清理边界，不代表 migration 执行墙钟或 FNOS CPU 收益。

### LUX-386 unchanged NFO 默认值修复查询调用

实现提交：`0f4dcdcc`。在 `uname -m=arm64` 的开发机上，用临时 SQLite 库、1 个媒体条目、1 个 unchanged NFO 文件和可用的 rich NFO cache 执行 `enrich_nfo_item`。旧路径为 4 次 storage query-wrapper 调用；新路径为 3 次，减少 1 次（25%）。减少的调用是默认值修复中的无变化 SELECT；默认值完整时也不再创建该修复事务。Storage query counter 统计应用层 query-wrapper 调用，不统计 BEGIN/COMMIT，也不测量执行时长。

回归命令：`CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib unchanged_nfo_with_complete_defaults_skips_repair_query`。另由 `tests/metadata.rs` 验证两种字段单独缺失时仍可修复并保留已有值。该结果只说明 1 项 SQLite fixture 的调用边界；没有测量墙钟、PostgreSQL、FNOS 或 NAS/x86 性能。

### LUX-392 扫描本地 metadata 完整度 source 读取调用数

在单 item、单目录 SQLite fixture 中，完整度阶段旧路径重新展开目录 source 并读取 active metadata，共 3 次 storage query-wrapper 调用；新路径复用 NFO 阶段的 `(item_id, source_id)` 快照，先批量确认首选 source 仍有效，再读取 active metadata，共 2 次，减少 1 次（约 33.3%）。回归同时覆盖首选 source 切换、文件标 missing 和 source 删除时拒绝旧快照。

该计数只覆盖完整度阶段的 source 与 item metadata 读取，不包括 NFO 阶段原有 source 查询、完整度计划、刮削器可用性、结果写入或 `FILL_MISSING` 调度；storage query counter 也不测量 SQL 执行时长。未据此推断墙钟、FNOS CPU、PostgreSQL 或 NAS 性能收益。

### LUX-393 插件卸载刮削器计数回归校准

后续合并后的卸载路径在同一双媒体库 fixture 中固定执行 6 次 storage query-wrapper 调用：配置读取、批量删除、批量重插、批量主刮削器更新、插件引用清理和插件记录删除。旧回归中的 8 次期望与当前实现不符，已按当前 fixture 更新为 6 次；无运行时代码变化。

### LUX-394 自动 FILL_MISSING 可选详情门槛

本地扫描完整度计划仍记录缺失的 `EXTERNAL_IDS` 和 `TRAILERS`，但二者单独缺失不再被当作自动 FILL_MISSING 的排队理由；核心 metadata、启用图片或 credits 仍会触发任务，任务触发后仍可顺带获取这两类详情。显式 metadata 任务继续使用通用 request plan。

回归使用固定的单电影 `StoredMediaMetadata` 测试对象验证仅缺 external IDs 与 trailer 时自动计划不可排队、缺少 credits 时仍可排队，且通用计划仍识别 optional 能力。该测试只证明本地计划决策，不测 SQL 调用、任务墙钟或生产 CPU；未据此推断 FNOS、PostgreSQL 或 NAS 收益。

### LUX-395 管理健康扫描任务计数

FNOS 上运行的 `pdzhou/lux:test` revision `bd32e0d`，PostgreSQL `scan_jobs` 约有 25.7 万行。旧的 `SUM(CASE...)` 单条查询使用并行顺序扫描，单次测量约 155 ms、读取 8,722 个 shared buffers；把活动状态单独统计时，现有活动部分索引的测量约 0.11 ms，而失败状态仍需要全表扫描。该基线来自线上旧版本，只描述当时查询成本。

当前变更将活动计数与失败计数拆开，并新增 `status = 'FAILED'` 的部分索引。SQLite 定向回归使用空库迁移和 4 条固定状态记录验证计数，并由 `EXPLAIN QUERY PLAN` 确认活动/失败查询使用各自的部分索引。验证命令为 `CARGO_TARGET_DIR=/Volumes/Toshiba/mywork/Lux/target cargo test --locked --lib scan_job_status_counts_use_covering_status_indexes`；开发机架构为 `arm64`。测试没有记录 SQLite 查询耗时；PostgreSQL 新迁移也未在 FNOS 部署或做线上复测，因此目前没有可报告的生产优化后收益。

### LUX-396 管理健康探测调用频率估算

管理员 dashboard 每 15 秒刷新一次；每个健康 payload 的低频探测包含一次数据库写探针、配置目录可用性/可写性检查（包括写入并 `fsync` 4 KiB 临时文件）和一次 `ffprobe -version` 子进程。若每次刷新都重新探测，单个持续打开的 dashboard 每分钟会触发约 4 组此类探测；按 AppState 合并并发请求并缓存 30 秒后，持续请求时约为每分钟 2 组，静态调用频率估算下降 50%。缓存仅在请求到达时刷新；CPU、连接池、任务计数、媒体库信息等动态字段仍逐请求读取。

该估算来自前端刷新间隔和服务端调用结构，不是运行时计数或 CPU 基准；没有据此推断 FNOS CPU、API 时延、PostgreSQL 或 NAS 性能收益。`/health/ready` 保留实时数据库写探针。

### LUX-397 DEFERRED FILL_MISSING 去重边界

provider 暂不可用会使 metadata job 进入 `DEFERRED`、相应 item 进入 `FAILED/SCRAPER_UNAVAILABLE`。此前扫描 completeness 调度和通用 FILL_MISSING 创建入口虽限制 1 小时内的 DEFERRED job，但又只选取 `PENDING/RUNNING` item，导致 provider-unavailable item 实际不受该窗口去重。现在两处都将 1 小时内此错误分类的失败 item 纳入既有去重判断；其他失败可以立即重试，provider-unavailable item 超过窗口后也可重新排队。

验证只覆盖固定 SQLite storage 状态机回归，确认两个入口的 enqueue/job 去重边界；没有比较运行时创建速率、provider 请求量、墙钟、FNOS CPU、PostgreSQL 或 NAS 性能，不据此推断生产收益。

### LUX-400 跨 job 的 FILL_MISSING worker 上界

源码中单个 `FILL_MISSING` job 的默认 worker 并发为 2，但原有进程级 metadata semaphore 容量为 16，因此多个同时执行的补缺 job 总计可能占用最多 16 个 metadata worker。现在所有补缺 job 共享容量为 2 的独立进程级 semaphore；每个 item worker 在处理期间同时持有该 permit 和既有 metadata permit，其他 metadata 模式不受补缺专用限制。

固定 SQLite fixture 同时运行两个各 4 个 item 的补缺 job，通过查询这两个 job 的 `RUNNING` item 数观测已 claim 的 worker 并发，最大值不超过 2。取消回归验证第二个 job 等待 permit 时，取消会在占用 permit 的首个 job 结束前让其转入 `CANCELLED`；单测也验证取消通知被消费后，之后的等待仍能观察到锁存状态。测试用异步 mutex 串行化同一测试二进制里会实际运行 FILL_MISSING 的用例，避免共享进程级 semaphore 产生测试相互干扰。`tests/reidentify.rs` 14 项、取消锁存单测、build、fmt 和全目标全 feature Clippy 通过。全目标测试的 library 部分为 734 passed、11 ignored，随后在独立的 `tests/emby_counts.rs:159` 失败（实际 1、期望 0）。以上仅验证并发上界与取消状态边界，不测量 CPU 或墙钟收益；没有在 FNOS、PostgreSQL 或 NAS 上部署和验证，不据此推断生产 CPU 收益。

### LUX-401 自动补缺请求快照与重复写入

自动 completeness 调度现在为每个 `FILL_MISSING` job item 保存输入 fingerprint 和规范化 capability 集；claim 时复制一份处理快照。重复且相同的请求只读当前任务状态，不执行 job/item 插入或更新。queued item 收到变化请求时批量更新既有 job item；running item 在当前 worker 结束后最多回到 `PENDING` 一次，并且该次不增加 `processed_count`。近期 provider-unavailable `DEFERRED` 只抑制完全相同的快照。

固定 SQLite storage 回归通过触发器计数确认：重复同快照请求对 job/item 表执行 0 次 INSERT、0 次 UPDATE；queued 中出现新 fingerprint 时执行 0 次 INSERT、1 次 item UPDATE、0 次 job INSERT。状态机测试还验证 capability 集变化、运行项只重跑一次、同快照稳定完成、取消不重跑、worker 失败后的显式 retry 使用最新 fingerprint、近期 DEFERRED 同快照去重和变化快照重新排队。该计数是单 item fixture 的 DML 边界，不是完整性流程 SQL 总数或墙钟基准；未测量数据库 CPU、FNOS、PostgreSQL 或 NAS 收益，也未部署。

### LUX-402 自动 FILL_MISSING provider 退避

自动补缺 item 在 provider 不可用后保存失败次数与下一次重试时间，延迟依次为 5 分钟、30 分钟并封顶 6 小时。相同 fingerprint/capability 快照在截止时间前命中现有 deferred item，不创建新 job；到期后新 job 继承此前失败次数，并在同一事务中将旧到期失败记录标记为已消费，避免重试成功后历史行再次解锁任务。新 fingerprint/capability 替代旧快照时也会消费失效的旧 provider 失败，避免新请求成功后再被旧快照触发。人工 retry 和新请求快照会重置该标记。快照改变、无快照任务和其他 metadata 模式保持原有语义。为使 READY 且仍缺失的完整度条目能在后续扫描中触发重试，每个最多 512 条的完整度检查批次会针对本轮仍缺失且自动匹配可用的候选 ID 查询到期失败项。该 `EXISTS` 查询只针对当前批次 ID，复用完整度 claim 事务，不扫描全库或增加事务往返；只在到期重试或找到被新快照取代的旧记录时，对相关旧失败行执行一次有界批量更新。

0165 对旧失败记录使用一个共享的首次冷却截止时间，不再批量更新 `metadata_reidentify_job_items`。SQLite 迁移回归确认旧 job/item 状态和计数不变且迁移没有更新历史 item 行；SQLite 扫描与状态机回归覆盖 READY 缺失状态到期后重新入队、冷却时间和失败次数继承、旧快照被新 fingerprint 取代后不再解锁。精确扫描回归通过，`tests/storage.rs` 49/49、`tests/scanning_jobs.rs` 81/81 通过；构建、fmt 和全目标全 feature Clippy 通过。最新全目标单测为 747 passed、0 failed、12 ignored；`emby_counts` 和 `strm` 仍在干净 `origin/test=8412cc2a` 基线复现失败，`libraries_api` 在全套运行中有一次数据库不可用，但单独测试及整个目标 13/13 通过。新增独立 PostgreSQL 用例验证新 fingerprint 成功后旧 provider 失败不再解锁任务，1/1 通过。此前 PostgreSQL 环境不可用；恢复 PostgreSQL 16.15 临时实例并修正测试夹具绕过 `Database` SQL 适配器、直接发送 `?` 占位符的问题后，`postgres_database` 16/16、progressive-scan storage contract 1/1 通过。以上是固定测试与 SQL 边界，不测量运行时 CPU、重试流量、墙钟、FNOS 或 NAS 收益。

### LUX-403 queued FILL_MISSING 容量复用

在固定 SQLite fixture 中，先生成 3 个 queued job（100、100、50 项），再增加 1 项时复用第三个 job，job 数保持 3；继续增加 80 项后，先填满第三个 job 再创建 31 项的第四个 job，最终分布为 100/100/100/31，共 331 项。旧逻辑会在最早 job 已满时另建 job，即使后面仍有空位。

该结果只证明有界队列分配的状态与 job 数；未测量多次扫描下的查询延迟、CPU、FNOS 或 NAS 收益。PostgreSQL progressive-scan storage contract 也验证了同库新 item 复用已有 queued job，但没有测量 PostgreSQL 查询延迟或生产并发争用。

### LUX-405 跨媒体库全量扫描串行队列

固定 SQLite 回归同时启动两个不同媒体库的全量扫描，共用同一个 `ScanJobService`，并给共享扫描工作 semaphore 配置 2 个 permit。数据库 trigger 会拒绝第二个 `RECONCILE_LIBRARY` 进入 `RUNNING`；两个 job 最终均完成，验证串行约束来自全量扫描队列，而不是只有一个扫描 permit。默认扫描并发仍为 2，全量扫描队列容量为 1。

该测试验证并发策略与跨库排队，不测量扫描墙钟、CPU、FNOS、PostgreSQL 或 NAS 性能，也不据此推断生产收益。

### LUX-406 扫描本地 NFO 阶段复用 source 与 metadata 读取

扫描图片阶段已经查询的 preferred source 快照现在传给 NFO 阶段；NFO 开始前通过有界 source-identity 查询排除已陈旧的 preferred source，写入前仍执行原有 freshness 复核。NFO 路径先按批次解析，数据库只批量读取确实存在 NFO 的条目 metadata，因此每个 NFO 不再单独读取完整 metadata 行；无 NFO 批次跳过 metadata 查询。批次 source 分组只复制图片/层级处理所需字段，不深拷贝完整 source 结构。家庭视频 NFO 路径检查失败仍会记录为条目错误。

固定 SQLite 回归中，两个带 NFO 的电影共用一条 metadata 查询和一条 source 身份校验；包含两个 NFO 的 NFO 阶段共执行 4 条 SQL（含两条 NFO 写入）。无 NFO 的一个 item 只执行一条 source 校验，不读取 metadata；preferred source 已变陈旧时也只执行一条校验并跳过 NFO 处理。`scan_local_metadata_nfo_retains_home_video_path_errors` 验证家庭视频路径不可访问时保留 I/O 错误。`scanned_metadata` 16/16、`scanning_jobs` 81/81 通过。以上是 SQL/行为边界，不是墙钟、CPU、FNOS、PostgreSQL 或 NAS 收益测量。

### LUX-408 扫描本地 metadata outbox 合并写入

2026-10-07 在 ARM64 开发机（`uname -m=arm64`）以同一确定性 60,000 文件 / 600 目录 fixture（SHA-256 `23de3a20c11c6a6e7cd44b76af7d1a84e85b9747e2ed2661668dbdf94dad9914`）交错运行基线与候选各三轮。基线为候选直接父提交 `9fb6abae`；为使未优化基线能完成报告，临时只放宽了基准中的 `<209` 断言，不改运行代码。候选将每个正向扫描提交中的多个 256-source outbox batch 合并为一条最多 64 行的多值 INSERT；SQLite 和 PostgreSQL 每条仍最多 384 个绑定参数。每轮均使用新空数据库；SQLite `synchronous=FULL`，PostgreSQL 为本机 Docker 16.15。表中首扫、重扫和 p95 均为毫秒，前台 p95 是扫描期间 50 个目录请求，目录列表 p95 与 batch p95 分别单独采集。

| 后端 / 版本 | 首扫索引：三轮 / 中位数 | 120k target 中位数 | 无变化重扫：三轮 / 中位数 | 前台 / 目录列表 / batch p95 中位数 | SQL / DML 中位数 | outbox DML | WAL 中位数 / 最大锁等待者 |
|---|---:|---:|---:|---:|---:|---:|---:|
| SQLite 基线 | 3,000 / 2,966 / 3,023；**3,000** | 593 | 1,667 / 1,678 / 1,692；**1,678** | 805 / 845 / 425 | 596 / 359 | 240 | 20,826,632 bytes / - |
| SQLite 候选 | 2,970 / 3,027 / 3,019；**3,019** | 605 | 1,768 / 1,771 / 1,652；**1,768** | 862 / 868 / 419 | 366 / 127 | 8 | 20,707,152 bytes / - |
| PostgreSQL 基线 | 6,290 / 6,166 / 6,219；**6,219** | 1,772 | 4,296 / 3,918 / 50,371；**4,296** | 878 / 1,011 / 875 | 570 / 335 | 240 | 246,728,655 bytes / 0 |
| PostgreSQL 候选 | 6,161 / 6,144 / 6,224；**6,161** | 1,745 | 50,468 / 3,543 / 4,239；**4,239** | 873 / 1,060 / 850 | 338 / 103 | 8 | 230,163,605 bytes / 0 |

固定 60k 扫描的 outbox DML 从 240 降到 8；总 DML 在 SQLite 从 359 降到 127，在 PostgreSQL 从 335 降到 103。SQL 中位数分别从 596 降至 366、从 570 降至 338。首扫和 target 墙钟中位数接近持平；这些数据验证的是批量写入和语句数下降，不证明 CPU 或 FNOS 收益。PostgreSQL 基线第 3 轮与候选第 1 轮的无变化重扫各有一次约 50 秒离群值，阶段记录均指向 `known_path_query`；该路径不在 LUX-408 改动范围，故保留原始值并只比较中位数，不将其归因于候选。修改尚未部署 FNOS，也不外推 NAS/x86_64 性能。

### LUX-409 unchanged NFO 写回调用边界

固定 SQLite 单 item fixture 首次 probe 写入 NFO 并启用 metadata mirror 时执行 4 次 storage query-wrapper 调用；紧接着重复完全相同的 probe，调用数由旧路径的 3 次降至 2 次。省去的是 unchanged NFO 路径的 mirror 策略读取；数据库 NFO 状态同步原本已只在文件变化时执行。未变化路径复用原子写入流程已读取的 `FileStamp` 生成 metadata fingerprint；回归确认其字节与原 stat 算法一致，并确认既有 mirror 内容不变。storage query-wrapper 不统计文件系统时延、BEGIN/COMMIT 或 SQL 执行时间；此结果不代表墙钟、FNOS CPU、PostgreSQL 或 NAS 收益。

### LUX-410 普通电影页图片登记边界

固定 SQLite 两电影 fixture 中，普通 `enrich_movie_library` 原先对每项各执行一次图片登记，加上来源页读取共 3 次 storage query-wrapper 调用；现在同页两项共享一个图片登记事务，共 2 次。每批最多 16 项，与 storage 接受的 item 上限一致。对其中一项注入图片 upsert 失败后，批事务回滚并按 item 重试，另一电影仍登记成功。此 fixture 只证明批次边界和错误隔离，不测量端到端墙钟、磁盘性能、PostgreSQL/NAS 争用或 FNOS CPU。

### LUX-411 电影 NFO 写回上下文读取

固定 SQLite 单电影 fixture 中，probe NFO 首次写回原先分开读取 source/context 与辅助字段，共 4 次 storage query-wrapper 调用；合并 context 后为 3 次，减少 1 次（25%）。修改电影 NFO 的 fixture 也验证单次读取 item type、source、sort title 和 added_at，变化写回总计 3 次调用（context、mirror policy、状态同步）。回归覆盖非电影和软删除条目不注入电影辅助字段，以及既有 sort title/dateadded 输出。

该计数来自应用层 storage query-wrapper，不统计 BEGIN/COMMIT、文件系统时延、SQL 执行时间或数据库往返；未测量墙钟、PostgreSQL、FNOS 或 NAS 性能，也不据此推断生产 CPU 收益。

### LUX-412 普通剧集页图片登记边界

固定 SQLite 两 series fixture（各一个 episode、各有一个 poster）中，逐 item `index_images` 路径执行 11 次 storage query-wrapper 调用；页级读取与最多 16 item 的批登记后执行 4 次，减少 7 次（约 63.6%）。新路径包括 source 页读取、已有图片页读取、图片 upsert 和 poster fallback 清理。trigger 注入首个 series 图片写失败后批事务回滚，再逐 item 重试；第二个 series 更新成功，失败项被单独记录。

该数字来自应用层 query-wrapper 计数，不含 SQL 执行时长或文件系统耗时；不据此推断墙钟、PostgreSQL、FNOS 或 NAS 收益。

### LUX-413 completeness credits relation 短路

本地 metadata completeness 计划先检查 NFO projection 是否同时包含导演和编剧。若 projection 不存在或任一列表为空，credits 已确定缺失，直接保留补全请求并跳过 `people.json` relation-existence 检查；只有两类 crew 信息齐全时才检查人物关系。候选模块单测覆盖缺失/完整 crew 判定。该回归验证决策边界，不统计文件系统调用总数或时延，也不代表 FNOS CPU 收益。

### LUX-414 图片阶段等待通知

扫描 job 等待本地 metadata 图片阶段时，原路径每秒重查一次数据库。现在 outbox worker 在成功保存图片阶段完成状态后通知同 job 的等待者；该等待者会重新读取数据库，1 秒 fallback 保留用于跨进程变更和恢复。固定 SQLite scanner 测试确认 pending batch 转为 completed 并发出通知后，等待在 250ms 内结束。未统计查询数或时延，不外推文件处理墙钟、PostgreSQL、FNOS 或 NAS 收益。

### LUX-415 idle metadata worker 查询

本地 metadata worker 在没有 pending target 时收到 Notify，原先查询 pending 状态后还会重读 scan job；固定 SQLite idle fixture 的一次唤醒是 2 次 query-wrapper 调用。现在 Notify 后直接重查 pending target，scan job 行只在 1 秒 fallback 读取，单次唤醒变为 1 次。该计数不含 SQL 执行时长，不外推 PostgreSQL、墙钟或 FNOS 收益。

### LUX-416 unchanged NFO content fast path

固定 SQLite 单 item fixture 原路径在 stat 指纹因 NFO 所在路径变化后会读取并解析 XML、复核 rich-cache fingerprint，再写 metadata：约 6 次 query-wrapper 调用。现在读取 NFO 字节并计算 SHA-256 后，与 rich cache 同查询取得的内容指纹比较；内容完全相同时且默认字段完整、actor relation 不需修复，只写回新的 stat fingerprint，3 次 query-wrapper 调用。原 stat 指纹匹配且默认值完整的 unchanged fixture 也因 cache JSON 与 fingerprint 单查询读取，从 3 次降为 2 次。后续标题内容变化回归仍进入解析与更新路径。此优化仍需读取全部旁车字节；query-wrapper 不代表文件系统时延、墙钟、PostgreSQL 或 FNOS CPU。

### LUX-417 completeness 页 NFO projection context

固定 SQLite 单电影 fixture 中，projection fallback 走 `read_item_projection(item_id)` 时会额外执行 2 次 query-wrapper 调用来读取 item kind 与 source path；复用 completeness 页已加载的 writeback context 后，projection 本身为 0 次额外 query-wrapper 调用。批量 context 查询已计入页级读取路径，测试单独比较 projection 阶段。NFO 文件仍需读取，source/media/directory 仍需 canonicalize 并验证 root 边界；未测文件系统墙钟、PostgreSQL、NAS 或 FNOS 负载。

### LUX-418 普通电影 NFO 并发上界

`enrich_movie_sources` 对每个最多 16 item 的来源页使用 `JoinSet`，最多同时运行 4 个 NFO enrichment；已有 task 完成后才领取下一 item，并按来源顺序合并 report。测试使用 12 个异步任务确认运行中的任务上限为 4，电影 fixture 验证一个坏 NFO不会阻断同页健康电影。此任务未改变 SQL 数量，也未测量墙钟；事务锁仍会约束写阶段并发。不能从本地调度上界推断 FNOS CPU 或 NAS 收益。

### LUX-419 普通剧集 NFO 并发上界

每个已加载的 series metadata page 会依层级遍历收集存在的 `tvshow.nfo`、season NFO 和 episode NFO，交给共享有界 helper，最多运行 4 个 enrichment task，再按发现顺序汇总结果。filesystem 的目录扫描和图片索引仍在原 hierarchy traversal 中执行，NFO 旁车的查找及读取没有消除。该记录只描述调度并发上限；没有采集 SQL/墙钟基准，也不推断 PostgreSQL、FNOS 或 NAS 收益。

### LUX-420 local metadata completion waiter

completion waiter 仍优先等待本地 worker 的进程内 `Notify`，未收到通知时的 fallback 查询间隔由 1 秒增至 5 秒；当前同进程完成路径会发出通知。跨进程数据库状态变化可见窗口相应为最多 5 秒。通知 fixture 确认 target 完成后在 250ms 内唤醒。没有采集总体 SQL 或墙钟数据，不据此外推 CPU 收益。

### LUX-421 completeness completion query reduction

固定 SQLite fixture 中，补全策略启用时的 completeness completion 路径由外层 READY/missing 筛选、有效媒体项筛选和请求构建器读取组成，合计 9 次 query-wrapper 调用。请求构建器的查询已限定同一 library、未软删除的有效媒体类型，以及 READY 且 missing 的 completeness capability；因此移除外层两次重复筛选后为 7 次。输入 eligible IDs、fingerprint 匹配、事务内锁定和 enqueue 逻辑保持。该计数只说明 fixture 的应用 query-wrapper 调用，不代表 SQL 耗时、真实往返时间、PostgreSQL 或 FNOS 收益。

### LUX-422 local metadata completeness planning concurrency

本地 completeness 页中的 item image discovery、必要时的 NFO projection 和 actor relation file check 从逐项串行改为最多 4 个只读 task 并行；结果在进入 attempt-state 查询前按原 item 输入顺序汇总。固定 12-task helper fixture 证明最大并发为 4 并且输出顺序稳定。此项不减少旁车文件总读取数或数据库 query 数；没有采集文件系统墙钟，也不推断 PostgreSQL、NAS 或 FNOS 收益。

### LUX-423 local metadata worker job-state fallback

本地 metadata worker 在无 pending target 且没有 Notify 时，scan job 状态 reload fallback 从 1 秒调至 5 秒；Notify 和 stop watch 仍即时唤醒。worker 初始读取失败的 retry 与 image-stage waiter 保持 1 秒。通知回归断言仍为一次 query；5 秒配置回归锁定状态查询间隔。该配置变化会把跨进程状态观察延迟扩大到最多 5 秒，没有测量总体查询数、CPU 或 FNOS 收益。

### LUX-424 扫描生成的 FILL_MISSING dispatcher

全量扫描、增量扫描以及 completeness completion 产生的 `FILL_MISSING` job 按 library 交给持久 dispatcher。每个已触发 library 保留一个 runner，同库 job 顺序执行；每个队列容量为 32，队列满时提交方等待，仍在队列或运行中的相同 job ID 会合并。提交先取得容量，再在 shutdown 状态锁内去重和入队，避免取消留下虚假的 pending ID，也避免关闭之后越过队列边界。关闭会唤醒容量等待者并拒绝新提交；已接收队列在 Tokio runtime 仍运行期间排空，关闭调用不会等待长 job。单个 job runner panic 会将该 job 标记为失败并继续处理后续队列。扫描路径不再为每个补全 job 建立完整 runner task；completeness 路径保留一个轻量完成观察任务以维持首页失效事件时序。

现有全局 `FILL_MISSING` item worker semaphore 仍限制最多 2 个 item 同时运行。测试锁定同库串行、32 项背压、重复 ID、背压取消后的重试、错误 library 路由、关闭唤醒、长 job 期间 shutdown 快速返回和单个 runner panic 后继续处理；这只说明调度上界，不统计总体 Tokio task 数、SQL、墙钟、CPU 或 FNOS 收益。管理员直接请求与计划任务入口仍待后续接入同一 dispatcher。

### LUX-425 管理员 FILL_MISSING 入口复用 dispatcher

管理员整库 reidentify、整库 metadata refresh 与单条目 metadata refresh 的 `FILL_MISSING` job 现在进入 LUX-424 的 library dispatcher；每个已触发 library 仍由一个 runner 顺序执行，队列满时 HTTP 提交等待容量。`FULL_REFRESH` 和 `REIDENTIFY` 保持现有 runner。dispatcher 关闭时管理员收到结构化 `503 DATABASE_UNAVAILABLE`；job 已先持久化，保持 `QUEUED` 供恢复后重试。

`tests/reidentify.rs` 15/15 通过，包含三种管理员入口的正常完成和 dispatcher 关闭时保留 queued job。此项不测量管理员请求速率、总体 Tokio task 数、SQL、墙钟、生产吞吐或 CPU，不推断 FNOS/NAS 收益；显式 retry 与计划任务路径仍待检查。

### LUX-426 管理员 FILL_MISSING retry 复用 dispatcher

管理员重试终态 `FILL_MISSING` job 后，按 job 持久化的 library ID 复用 LUX-424 dispatcher；`FULL_REFRESH` 与 `REIDENTIFY` 继续走原 runner。dispatcher 关闭时返回结构化 `503 DATABASE_UNAVAILABLE`，已重排的 job 保持 `QUEUED`。回归曾在旧路径观察到关闭后仍返回 `202`，修复后验证拒绝提交及持久化状态。

该项只验证路由和 job 状态边界，不测量任务总量、吞吐、墙钟、生产 CPU 或 FNOS/NAS 收益；计划任务仍待检查。

### LUX-427 计划 metadata job 复用 dispatcher

`METADATA_PARSE` 计划任务创建的 `FILL_MISSING` job 现在经 library dispatcher 入队，并等待有界队列容量；dispatcher 错误作为调度任务启动错误返回。关闭回归验证计划任务报告失败且新 job 保持 `QUEUED`，可用路径回归仍返回原 metadata job。

此项不测量任务总数、调度延迟、吞吐、墙钟、生产 CPU 或 FNOS/NAS 收益；thumbnail scraper retry 属于另一条需要等待结果的路径。

### LUX-428 thumbnail scraper retry 复用 dispatcher

thumbnail retry 创建的 `FILL_MISSING` job 进入 library dispatcher。首个 enqueue 等 dispatcher completion；storage 把请求合并到已排队 job、dispatcher 未提供新 completion receiver 时，以 1 秒间隔读取 job 终态。只有终态后才检查 poster/thumb 并更新 retry attempt。enqueue 失败时释放 lease、保留尝试次数并安排内部错误恢复时间。

固定测试 gate 住 provider search，确认 job 运行期间 retry 仍为 `RUNNING`、attempt count 不变；provider 放行并且 metadata job 终态后才推进 retry。关闭 dispatcher 回归确认 job 留在 `QUEUED`、retry 恢复为 `PENDING` 且次数不变。该测试不测量真实任务时延、请求吞吐或生产 CPU，不推断 FNOS/NAS 收益。

### LUX-429 FILL_MISSING idle coalescing window

按 library 的 dispatcher 空闲并收到首个 job 后等待 1 秒，再 claim；这段时间里后续条目请求仍可被 storage 合并进相同的 `QUEUED` job，dispatcher 对重复 job ID 不重复排队。dispatcher 有积压时，后续 job 按序立即执行；队列再次为空后收到的新 job 才重新等待 1 秒。`RUNNING` job 的输入不变。此策略使空闲后的首个补全 job 最多额外等待 1 秒；固定回归验证了 storage 合并和 runner 时序，但没有测量真实请求批次、SQL、吞吐、生产 CPU 或 FNOS/NAS 收益。

### LUX-430 scan-run 等待通知与 scan-lock 退避

删除媒体库时，进程内 active scan run 的最后一个 guard 结束后通过 `Notify` 唤醒删除等待者；等待者在检查 registry 前先注册通知，因此 guard 即使恰好在注册与检查之间释放，也会被观察到。原 30 秒删除超时和阻止新 run 的 library fence 保持不变。扫描优先级与 manifest materialization 的数据库条件仍需跨进程轮询，间隔按 10、20、40、80、160、250ms 增长并封顶；相应的进程外状态变化观察窗口最多 250ms。拿到 scan semaphore 后仍复核数据库条件。

单测覆盖退避边界，删除回归覆盖终态 job 对应的活跃 run 等待、释放后完成和 fence。没有采集总体 SQL 数、实际等待时长、CPU、FNOS 或 NAS 性能数据。

### LUX-431 人物 manifest 跨进程锁退避

争用中的人物 relation/manifest 文件锁等待改为 10、20、40、80、160、250ms 并封顶，使用 1 秒单调截止时间；未争用时仍立即取得 `create_new` 锁，超过 300 秒的 stale lock 继续清理。固定本地回归确认退避边界、未占用锁成功和持续占用锁返回 `TimedOut`。最大锁等待合同仍约 1 秒，理论轮询次数由 100 次降至约 8 次；没有测量真实文件系统耗时、并发冲突率或生产收益。

### LUX-432 人物 relation 与 credits revision 一致性

当 relation 旁车 fingerprint 与请求匹配时，有数据库的 PeopleService 还查询 `person_index_item_state`，只有相同 source fingerprint 和 relation schema 才跳过同步。文件写入成功而 credits/index 更新失败时，下一次扫描会重试 credits；无数据库的服务仍只依据 relation 文件。该自愈检查每次已匹配 relation 增加一次应用层数据库查询；没有将 JSON 文件与 SQL 放入同一原子事务，也没有测量吞吐或墙钟收益。

### LUX-433 人物 credits 索引恢复写事务

人物索引重建仍按最多 100 个 item 读取关系、批量解析身份；credit replacement 改为每 16 个 item 一个 metadata write transaction 和 credits 锁，一个 100-item 页最多 7 个事务，避免过去逐 item 最多 100 次事务。单个 chunk 内的 SQL 失败会回滚该 chunk；此前已提交的 chunk 保留 fingerprint，重试时可跳过已完成 item。每 item 的已有 credits 查询和 DML 仍保留，所以这不是总 SQL 调用数对比，也没有 PostgreSQL 时延、锁等待、FNOS/NAS 或 CPU 测量。

### LUX-434 person manifest restore pending no-op

重复将相同 schema 的 restore state 标记为 `PENDING` 时，UPSERT 不再更新数据库行和 `updated_at`；`COMPLETED` 状态或 schema 变化仍执行 UPDATE。trigger 固定回归确认 UPDATE 为 0 次或 1 次。这不跳过调用方的 storage 查询入口，也没有测量生产更新频率、事务耗时或数据库收益。

### LUX-435 本地 NFO 页 actor credits 批次边界

本地 scan metadata NFO page 原先在每个 item 的 relation 文件写完后分别获取 credits 写锁并提交事务。现在先完成页内 relation 文件处理，再每 16 个 item 调用一次批量 credits replacement；含 N 项待提交 actor relation 的页面最多执行 `ceil(N/16)` 次 credits replacement 事务。固定回归覆盖两个 relation 先于 flush 写入、flush 前数据库 revision 仍为 stale、flush 后恢复 current；注入 credits SQL 错误时同一 16-item chunk 的 credits/index state 整体回滚，扫描报告该 chunk item 失败，其他 NFO 文件解析错误仍按 item 隔离。

该记录描述调用和事务边界，不是 SQL 执行次数或事务耗时测量。普通人物 API、在线刮削和单条目 enrichment 保持即时持久化；未测量 PostgreSQL 时延、锁等待、FNOS/NAS 或 CPU 收益。

### LUX-436 PostgreSQL query statistics 诊断

数据库诊断报告现在可选读取 `pg_stat_statements` 当前数据库的最多 20 条累计高执行时间语句，包含 query ID、调用数、总/均值/最大执行时间、行数和 shared block hit/read，不返回 SQL 文本或参数。extension 未预加载、未安装、无权读取或查询超时会返回不可用状态；SQLite 标记为不适用。该改动没有测量或改善任何 SQL 延迟；返回值受 extension 启用时间及 `pg_stat_statements_reset()` 影响，只用于后续定位热点。

### LUX-437 本地 NFO 页人物 manifest restore 标记

固定 SQLite 回归中，同一个 deferred NFO page context 并发持久化两个有变化的人物 manifest，restore-pending storage 调用由每人一次合并为每页一次；两个 manifest 均保留各自新 checksum。重复 checksum 为 0 次标记调用。数据库标记先于原子文件写入，因此原子写失败后 PENDING 状态仍保留供恢复。普通人物 API 和非 NFO 写入继续每次即时标记。

这里只统计该状态标记的应用层 storage query-wrapper 调用边界；manifest 文件读取、人物锁、原子写入、后续 credits flush、SQL 执行时间和墙钟均未计量，不外推 PostgreSQL、FNOS/NAS 或 CPU 收益。

### LUX-442 completeness claim/commit transaction boundary

本地 metadata completeness 的每个最多 512-check 批次现在通过一次 storage 调用，在同一个 metadata write transaction 中 prepare/claim、提交新结果、筛 due provider retry，并创建或合并补缺 job。旧路径先执行一个 claim transaction；有结果或 due retry 时再执行一个至六个 completion/scheduling transaction（FILL_MISSING 查询和写入仍按每 100 项分片）。因此需持久化的批次由 2–7 个 transaction/acquire 周期变为 1 个；READY 且 fingerprint 未变、没有 due retry 的批次旧路径已只执行一次 claim transaction，新路径仍为一次合并 transaction。结果行在 storage 中仍以最多 100 条 SQL 分片更新，job 输入仍保留 100-item 分片。

SQLite 故障注入在 job row 已插入、job item 写入失败时确认 completeness claim/READY 变化和 job row 全部回滚；移除故障并由独立 database handles 并发重试后只提交一份结果及一个 job。另在 due provider retry 已进入消费流程后注入 job item 写入失败，确认 `automatic_retry_consumed`、job/item 和 completeness 状态全部回滚。due retry 候选查询也按 100 个 item 分片；205 项固定 fixture 的 query-wrapper 总数从未分片时的 11 次变为 13 次，其中 due retry 查询为 3 片。这是超出单片大小时有界查询的代价，不表示查询变快。该记录描述事务边界和固定回归，不代表 SQL 执行调用数下降、事务墙钟缩短或 PostgreSQL/FNOS/NAS CPU 收益。新 transaction 会连续持有同一 metadata write lock 完成最多 512 项工作；真实 contention 与时延仍需在 PostgreSQL/FNOS 负载下测量。LUX-421 的 9→7 query-wrapper 计数仍仅描述其原 completion 子路径，不是此合并路径的端到端 SQL 基准。

### LUX-443 本地 metadata worker 合并状态和 target page 查询

worker 现在一次 storage query 同时读取 pending 状态与 MOVIE、VIDEO、EPISODE 优先级下首个有界 source page；每个类型候选最多取请求页大小，再按 target ID 选首个可用类型，因此无 source 的早期 pending 项不会挡住后续可处理类型。固定 SQLite 回归覆盖每类 page 顺序、无 source pending 与无 pending，storage query-wrapper 次数各为 1。原处理循环最多执行 pending 检查 1 次和类型 page 查询 1–3 次；新循环为 1 次组合 page query。Manifest worker 校验也将 workflow/discovery 状态与全部 root identity/target cursor 信息由 2 次 storage query 合并为 1 次；仍会在有 pending 的批次逐个 stat 有正向记录的本地 roots，根身份变化时 worker 不解析或写回 NFO，并将 target 保留为失败可重试。

固定 scanner 回归确认 root mismatch 会让 target 进入 `FAILED`、library root 标为不可用且媒体条目不被本地 NFO 改写；既有空闲 Notify 回归仍是一条 query-wrapper 调用。metadata 20/20、scanned metadata 16/16、scanning jobs 81/81 及 PostgreSQL progressive-scan storage contract 通过。本机 `arm64`；这些计数不代表 SQL 墙钟、PostgreSQL 生产负载或 FNOS/NAS CPU 收益，也未做部署测量。

### LUX-444 completeness page 本地 projection 与 relation 检查缓存

固定测试 fixture 中两个 episode 指向同一 canonical `episode.nfo` 时，共享的 page-scope `OnceCell` 仅允许一个并发调用初始化读取/解析，其余 item 复用解析结果；下一页建立新 cache，会观察 sidecar 内容更新。NFO path/read/parse error 在 cache 中保留对应的 `NfoWriteError` 类别，并由 LUX-422 的有序结果汇总选择首错误。数据库已提供可解析 NFO projection 的 item 不进入旁车缓存路径。

actor relation 检查仍先逐 item 检查新式 relation。需要 legacy fallback 时，一页只枚举一次 `people/items`，最多读取 4096 个 directory entry；缓存完整目录内的缺失 ID 时跳过 legacy per-item stat，只为目录中命中的 ID 读取 relation JSON。超过上限或目录读取失败会对未确定 item 回退原路径检查，因此旧目录很大时不会无界遍历。固定回归确认共享 legacy listing 只初始化一次、命中的 relation 仍被解析，以及不可读目录触发逐路径 fallback。

该项复用现有每页最多 4 个 item 的并发上限；路径 canonicalization、新式每 item relation 检查和图片 discovery 仍各自执行。未统计真实目录大小、旁车字节数、文件系统调用总量或耗时；性能描述只基于固定行为回归，不推断墙钟、PostgreSQL、FNOS/NAS 或 CPU 收益。

### LUX-448 相同人物图片与 provider index 的 no-op 写入

重复上传同一 profile 图片的固定回归中，旧路径会替换人物图片及两个 TMDb index 文件；候选路径在 inode 保持不变时复用现有文件。不同字节与缺失文件仍经原子临时文件写入。相同内容路径仍会读取同长度目标并校正私有权限，因此这里记录的是跳过临时文件写入、`fsync` 与 `rename` 的行为，不代表文件系统总调用或墙钟下降；没有测量图片下载请求、FNOS/NAS、生产 I/O 或 CPU 收益。

### LUX-449 本地 NFO metadata 状态写事务

本地 NFO page 的 metadata/provider ID/premiere date/fingerprint 更新按最多 16 个 item 共用一次 metadata 写锁和事务；identity conflict 检查会在标题或年份变更前刷新此前暂存项，因此部分批次会早于 16 项提交。16 个 item 的 SQL `UPDATE` 语句仍分别执行。跨 item 故障回归证明批次事务在后续 UPDATE 失败时整体回滚；应用层随后逐 item 回退，以保留单条错误隔离。该记录说明事务和锁的合并边界，不声称 SQL 调用数减少、事务墙钟缩短或 PostgreSQL/FNOS/NAS CPU 改善。

### LUX-450 本地 NFO 默认字段修复与 metadata 状态批次

已检查且内容未变化的本地 NFO，在 rich cache 命中但 provider ID 或 premiere date 为空时，会把默认值修复暂存到当前 page state batch；最多 16 个 metadata 更新/默认修复共用一次 metadata 写锁和事务。固定 SQLite 回归确认默认字段只补空值、已有非空字段不被覆盖、无变化不执行 UPDATE，且混合状态批次中途失败会整体回滚。应用层仍逐 item 回退并保留 NFO 错误隔离。

事务指标固定记录 transaction 次数、输入 item 数、metadata/default-repair 分类、成功/失败及最近 128 个耗时样本的 p95；回归确认失败 page transaction 和逐项 fallback 均被计入，且不暴露 item/path/SQL。这里记录的是事务尝试与输入批次形状，不代表 SQL 语句数减少或事务墙钟下降；未验证 PostgreSQL/FNOS/NAS 负载或 CPU 收益。

### LUX-457 本地 scan NFO sidecar 路径检查缓存

本地 metadata page 按完整候选 `PathBuf` 缓存 episode NFO sidecar 的 `try_exists` 结果，仅覆盖当前 page。三 episode 固定回归中，两个 item 复用同目录 `episode.nfo`，第三个 item 仍选用优先级更高的同名 NFO；四个不同 episode sidecar 候选路径各检查一次，共用 sidecar 不重复检查。LUX-458 同时让 hierarchy NFO 候选使用该 page cache。filesystem error 继续按原 `Option` 语义终止该 item 的 fallback。缓存不保留到下一页、不复用解析后的 XML，也未测量实际系统调用墙钟、NAS/FNOS 或 CPU 收益。

### LUX-458 本地 metadata NFO 路径发现有界并发

scan-local NFO page 的路径发现从逐 source 串行改为最多 4 个并发任务，最终仍按 source 输入顺序写入 snapshot。每个 page 用异步单元格共享相同候选路径的 in-flight `try_exists` 结果，覆盖 episode、series 和 season NFO。固定 12-episode fixture 观测到并发大于 1 且不超过 4；12 个不同同名候选加 1 个共享 `episode.nfo` 共进行 13 次候选存在性检查，shared fallback 仅检查一次。另一个两 episode fixture 让不同 hierarchy ID 指向相同 tvshow/season NFO，共 5 个唯一候选只进行 5 次检查。NFO 内容读取/解析、series/season 选择、每 item 错误映射及后续 credits transaction 边界不变。该回归测量的是应用层候选检查次数和并发上界，不是物理磁盘 I/O 数、文件系统墙钟或 FNOS/NAS/PostgreSQL/CPU 收益。

### LUX-459 NFO 完整语义 fingerprint

NFO cache 保留原始字节 SHA-256 快路径。只有配置了 local NFO cache 的 enrichment 才收集 semantic tokens；没有 cache 的普通路径继续使用既有 projection parser，不额外分配 token vector 或计算摘要。文件内容字节变化后，后台 enrichment 仍读取文件并在一次受大小与事件数限制的 XML pass 中提取 projection、人物和完整语义 fingerprint；XML 声明、注释、属性顺序、CDATA/文本表示、空元素写法及 element-only 格式空白不改变 fingerprint。所有元素名、属性和值、混合内容文本和未知 XML 子树仍参与摘要。文本与属性值都按 XML 1.0 的换行语义规范化；属性字面 TAB/换行折叠为空格，而 TAB/换行字符引用保留其字符值。合法的预定义/数字引用会参与 cache-enabled metadata projection 和摘要，避免投影与语义摘要对等价文本的判断不一致。`xml:space="preserve"` 下的空白会进入摘要，混合内容中的空白也会保留。仅当完整摘要匹配、rich cache 与其基于旧 source revision 的 actor relation snapshot 均有效且默认字段无需修复时，才持久化新的 raw content/stat revision 并跳过 metadata/credits 写入。旧版 details-only cache 在 raw 内容改变后完整处理并升级；相同原始字节继续走现有免解析路径。

回归覆盖 lexical equivalence、未知 XML、旧 cache 升级、stat/raw cache revision 更新、DTD/格式错误和未声明实体拒绝，以及文本和属性空白边界。实现与回归已写入，但截至 2026-10-09 尚未运行 Cargo 测试、Clippy 或性能测量；此处没有可报告的运行时收益数据。后续通过的正确性回归只证明解析后的重复写入可跳过，不证明文件读取或 XML 解析减少，也不把本机时间推断为 PostgreSQL、NAS/x86_64 或生产 CPU 收益。
### LUX-460 scan-local 普通电影 NFO 有界并发

scan-local movie metadata page 复用有序 task runner，最多同时执行 4 个普通电影 NFO enrichment；结果按 source 顺序归并，单 item 的解析或 task 错误继续隔离。page deferred metadata 与 actor credits 仍由原 page 末 flush 边界提交。title/year 发生变化的电影在共享 guard 下检查数据库现存身份及尚未 flush 的同页 pending identity reservations，再写入 NFO cache/actor relation 并排入 metadata update collector；入队后释放 guard。pending reservation 候选由 storage 按当前 parent、可用 source 和 active 状态复核，身份冲突检查不提前 flush page collector。同步 gate 固定回归检查冲突检查期间没有 metadata transaction，以及普通 metadata update 与唯一成功身份更新在 page 末共用一次事务。

这里记录的是应用层并发上限和身份检查/入队顺序；没有墙钟、吞吐、FNOS CPU、PostgreSQL 或 NAS 测量。Cargo 验证尚待统一测试窗口，当前记录不代表验收通过。
