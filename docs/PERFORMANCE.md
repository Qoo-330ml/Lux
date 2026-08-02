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

## 规则

- 首次扫描、无变化重扫、单目录增量、50 并发短 API 请求、扫描并发前台、4 个 Range 连接和任务恢复都要有独立记录。
- 每次性能优化记录硬件、数据集、命令、提交以及前后结果。
- 记录中的路径、token、真实外部 URL 和用户数据必须脱敏。
