# LUX-198：Web 播放会话、服务端 HLS 与 Jellyfin FFmpeg 7

## 交付状态

已完成。当前实现保留 Direct Play 优先级，并为本地媒体增加 Web 专用播放会话、0～4 档播放决策和会话级
fMP4/CMAF HLS；`.strm` 仍是 Direct-only。运行时使用 Jellyfin 官方项目发布的 FFmpeg 7 正式版
`v7.1.4-3`，不安装普通 Debian `ffmpeg`。

## 实施切片

1. **运行时**：按 `TARGETARCH` 安装固定的 Debian Trixie ARM64/AMD64 包并校验 SHA-256；容器 PATH 优先使用
   `/usr/lib/jellyfin-ffmpeg`。
2. **播放合同**：定义档位 0～4、`DIRECT`/`SERVER_HLS`/`UNSUPPORTED` 判别联合，以及浏览器能力参与的纯决策函数。
3. **Web 会话**：新增会话创建、短期签名 Direct URL、幂等事件、心跳、停止和资源生命周期接口；旧播放进度接口保持兼容。
4. **服务端 HLS**：本地媒体使用 fMP4/CMAF manifest、初始化片段和 `.m4s` 分片；每个会话独立进程组、stderr drain、并发信号量、
   临时目录配额、低磁盘拒绝、超时回收和重启孤儿目录清理。
5. **Web 引擎**：接入原生 Direct、Safari 原生 HLS、MSE/HLS.js，并保留已有客户端 HEVC/MKV fallback；播放器上报播放、暂停、
   停止，页面关闭和自然结束会释放会话。

## 播放规则

- 档位 0：原始 Range 直放；客户端解码/Remux fallback 仍属于客户端侧档位 0。
- 档位 1：视频和音频 copy 的 fMP4/CMAF HLS Remux。
- 档位 2：视频 copy、音频转码的 HLS。
- 档位 3：管理员配置且运行时确认可用的硬件视频转码 HLS。
- 档位 4：Jellyfin FFmpeg 软件视频转码 HLS。
- 本地媒体按 0 → 1 → 2 → 3 → 4 选择最低成本可用计划。
- `.strm` 只能返回档位 0；直连失败返回 `STRM_REQUIRES_DIRECT_PLAY`，不运行 `ffprobe`/`ffmpeg`，不生成 HLS 目录，
  不代理媒体字节。

## API 合同

- `POST /api/v1/playback/sessions`
- `GET|HEAD /api/v1/playback/sessions/{sessionId}/direct`
- `GET|HEAD /api/v1/playback/sessions/{sessionId}/hls/{asset}`
- `POST /api/v1/playback/sessions/{sessionId}/events`
- `POST /api/v1/playback/sessions/{sessionId}/heartbeat`
- `DELETE /api/v1/playback/sessions/{sessionId}`

创建和状态接口使用 Web session/CSRF；媒体资源只使用绑定会话、资源名称和过期时间的短期签名 URL。清单 URI 会被改写为同一
会话的签名资产，不能跨会话访问。事件通过 `eventId` 与 `sequence` 幂等，乱序事件不会倒退位置；`STOPPED`、页面关闭、自然结束、
心跳超时和服务重启均触发资源回收。

完整请求/响应字段见 [`docs/API.md`](API.md)；架构取舍见
[`ADR-004`](decisions/004-direct-play-only.md) 和
[`ADR-026`](decisions/026-web-playback-and-server-hls.md)。

## 验证记录

已完成以下验证：

- Rust：格式化、构建、全目标测试、Clippy；新增进程组回收回归测试先失败后通过。
- Web：冻结依赖安装、Vitest/Node 测试和生产构建。
- 数据库：空 SQLite 迁移通过；临时 PostgreSQL 16 容器中 3 个 ignored 集成测试全部通过，包含空库迁移、核心状态和 STRM 任务路径。
- 容器：ARM64 实际运行 Web 播放容器；AMD64 runtime/application 镜像按对应 Jellyfin 包构建并做版本/架构 smoke。
- 浏览器：真实浏览器取得 Direct Range 206、HLS manifest/init/segment，完成 seek、暂停、停止、自然结束和页面离开回收；`.strm`
  不创建 HLS 目录。
- 本机 `uname -m=arm64`；验证结果不用于宣称 NAS/x86_64 的性能或所有浏览器/编码格式兼容性。

详细兼容性证据见 [`docs/COMPATIBILITY.md`](COMPATIBILITY.md)。
