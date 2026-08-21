# LUX-033：ffprobe 媒体信息实施计划

## 范围

在 LUX-031 已持久化的本地媒体源上运行受控的 `ffprobe`，保存容器、时长、码率和视频/音频/字幕轨道。探测不进入 HTTP 请求路径；扫描 worker 在一次扫描完成后调用应用服务，随后通过媒体源和 Emby `PlaybackInfo` 提供结果。`.strm` 不属于本任务的主动源探测范围。

## 规则

- 命令使用参数数组调用，不经过 shell；输入路径作为单独参数传递。
- 输出只接受受限 JSON，忽略未知字段；stdout 有大小上限，避免异常工具输出耗尽内存。
- 默认超时 30 秒；超时、非零退出码、无法启动和坏 JSON 分别转成可查询的探测状态/错误。
- 仅对 `PENDING` 媒体源探测；成功后变为 `READY`，失败后变为 `FAILED`，同一轮重复执行不会重复探测。
- 文件大小或修改时间变化时，扫描器将媒体源重新置为 `PENDING`。
- `STRM_URL` 不在普通扫描中调用 `ffprobe`；存在同名 `-mediainfo.json` 时只解析旁车。管理员显式创建 STRM 探测任务后，受监督的 `media_probe` 插件可按原始目标读取 HTTP/HTTPS、本地路径、SMB 或 FTP 媒体；不支持的协议和不可访问目标保留失败状态，不阻塞媒体索引与播放表面。
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

### Slice 3：扫描完成后的后台接入

- [x] 扫描 worker 完成库扫描后探测仍为 `PENDING` 的本地媒体源；`.strm` 仅处理可用旁车，不主动探测外部源。
- [x] API 发起的扫描、重试扫描和启动恢复扫描均复用同一探测路径。
- [x] 探测结果写入媒体源并由 Emby `PlaybackInfo` 返回运行时长和媒体流；失败按文件隔离，不把整次扫描标记为失败。
- [x] 记录 `PROBE_COMPLETED`/`PROBE_FAILED` 扫描事件，并提供 ARM64 容器端到端冒烟脚本。

## 验证门

- `cargo test --locked --all-targets`
- `cargo fmt --all -- --check`
- `cargo clippy --locked --all-targets --all-features -- -D warnings`
- ARM64 本机验证新增/变化文件会探测，第二次不重复，失败状态可持久化。

验证结果：以上检查均通过；本机 `arm64` / `aarch64-apple-darwin` 已用受控 ffprobe fixture 验证新增、变化、成功、退出失败、超时和重复跳过。`LUX_IMAGE=lux:arm64-local ./scripts/probe-smoke.sh` 另验证真实 ARM64 容器扫描有效 MP4 后 `probeStatus=READY`、`durationTicks=10000000`、Emby `PlaybackInfo.RunTimeTicks=10000000`、2 条 `MediaStreams` 和 `PROBE_COMPLETED` 事件。

## 明确不做

- 不在 HTTP 请求路径中启动 ffprobe。
- 不保存原始 ffprobe JSON 或未限制的 stderr/stdout。
- 不对 `.strm` 指向的外部媒体运行 ffprobe；首次播放由客户端直接访问外部地址。
- 不在本阶段实现章节扫描或字幕文件扫描；外部字幕探测和 Emby MediaStreams 映射仅覆盖当前媒体源能力。
