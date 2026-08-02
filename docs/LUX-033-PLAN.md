# LUX-033：ffprobe 媒体信息实施计划

## 范围

在 LUX-031 已持久化的本地媒体源上运行受控的 `ffprobe`，保存容器、时长、码率和视频/音频/字幕轨道。探测不进入 HTTP 请求路径；本阶段提供可复用的应用服务和持久化能力，后续任务再接入后台任务调度。

## 规则

- 命令使用参数数组调用，不经过 shell；输入路径作为单独参数传递。
- 输出只接受受限 JSON，忽略未知字段；stdout 有大小上限，避免异常工具输出耗尽内存。
- 默认超时 30 秒；超时、非零退出码、无法启动和坏 JSON 分别转成可查询的探测状态/错误。
- 仅对 `PENDING` 媒体源探测；成功后变为 `READY`，失败后变为 `FAILED`，同一轮重复执行不会重复探测。
- 文件大小或修改时间变化时，扫描器将媒体源重新置为 `PENDING`。
- 每次成功探测在短事务中替换该媒体源的轨道记录，避免半套数据。

## 增量任务

### Slice 1：探测输出模型与数据库

- [x] 新增 `media_streams` 和探测错误字段迁移。
- [x] 实现受限 ffprobe JSON 解析和秒数到 Emby ticks 的转换。
- [x] 实现 ffprobe 子进程、超时、退出码和输出错误分类。

### Slice 2：变化检测与持久化

- [x] 扫描器比较文件大小/修改时间，变化时重置 probe 状态。
- [x] 保存容器、时长、码率和视频/音频/字幕轨。
- [x] 成功、失败、超时和重复运行均有测试覆盖。

## 验证门

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- ARM64 本机验证新增/变化文件会探测，第二次不重复，失败状态可持久化。

验证结果：以上检查均通过；本机 `arm64` / `aarch64-apple-darwin` 已用受控 ffprobe fixture 验证新增、变化、成功、退出失败、超时和重复跳过。

## 明确不做

- 不在 HTTP 请求路径中启动 ffprobe。
- 不保存原始 ffprobe JSON 或未限制的 stderr/stdout。
- 不在本阶段实现后台任务队列、章节、字幕文件扫描或 Emby MediaStreams DTO。
