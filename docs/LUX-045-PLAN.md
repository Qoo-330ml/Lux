# LUX-045：60k 扫描 fixture 与基准实施计划

## 范围

用不提交媒体二进制的确定性生成器建立 60,000 文件目录，固定测量首次扫描、无变化重扫、单目录增量以及扫描期间管理员前台 API 请求，并将 ARM64 本机结果记录到 `docs/PERFORMANCE.md`。

## 实现

- [x] 新增 60k 默认、可调文件数和目录数的确定性 fixture 生成器。
- [x] 新增 root 校验的单目录扫描入口，避免增量测试退化为全库扫描。
- [x] 新增被忽略的性能集成测试和固定运行脚本。
- [x] 在 ARM64 本机执行固定命令并记录真实结果。
- [x] 根据结果记录性能目标差距，不虚报 x86/NAS 性能。

## 验收证据

- 生成器单测验证重复运行的 manifest、路径和文件内容稳定。
- `scripts/run-performance.sh` 生成临时 60k fixture，执行 release 模式性能测试并打印 JSON 结果。
- 性能测试断言首次扫描、无变化重扫和单目录新增数量；确认扫描未触发 NFO/ffprobe（metadata fingerprint 仍为空、probe 仍为 `PENDING`）。
- 性能测试同时发起 50 个管理员库列表请求，记录 p50/p95 和错误数。

结果：2026-08-02，提交 `740de3c`，本机 `arm64` / `aarch64-apple-darwin`；首次扫描 14,104 ms，无变化重扫 4,061 ms，单目录 200 文件增量 31 ms；扫描期间 50 个管理员请求 p95 4 ms、错误 0，且 `foregroundDuringScan=true`。详细记录见 `docs/PERFORMANCE.md`。

## 明确不做

- 不提交 60k 个媒体文件或用户数据。
- 不把本机 ARM64 数字当成目标 NAS/x86_64 的正式容量结论。
