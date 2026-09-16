# LUX-254：Emby 客户端服务端转码

## 目标

让使用第三方 Emby 客户端的用户在本地媒体无法 Direct Play 时，能够通过 Emby 标准
`PlaybackInfo` POST 协商 Lux 服务端的 HLS 转码，并读取实际的清单、初始化片段和媒体片段。
复用 LUX-198 的 FFmpeg/HLS 实现和 `web_playback_sessions` 表，不让第三方客户端接触 Web session/CSRF。

## 设计假设

1. Emby 客户端通过 `POST /Items/{itemId}/PlaybackInfo` 的 `PlaybackInfoRequest` 表达播放能力。
2. `EnableDirectPlay=true` 时默认优先返回现有直放能力；POST 明确要求转码且
   `EnableDirectPlay` 未设置或为 `false` 时创建 Lux 服务端 HLS 会话。若客户端同时允许直放和转码，
   则按标准 `DeviceProfile.DirectPlayProfiles` 匹配本地媒体源；只有容器和音视频 codec 信息已知且确认直放 profile
   不匹配、同时存在 HLS `TranscodingProfiles` 时才创建服务端 HLS 会话。缺失/待探测元数据视为未知，不当作不匹配；
   没有顶层布尔值时同样按 `DeviceProfile` 协商。明确设置 `EnableTranscoding=true` 并禁用直放或 `forceTranscode=true`
   仍按客户端明确选择启动转码。
   POST 查询参数 `forceTranscode=true` 可覆盖直放请求，兼容把强制转码选项放在 URL 上的第三方客户端。
3. HLS 输出继续使用现有 fMP4/CMAF 资产，Emby 对外声明 `TranscodingSubProtocol=hls`、
   `TranscodingContainer=mp4` 和 `TranscodingMimeType=video/mp4`。
4. `.strm` 是 Direct-only；本任务不扩大它的服务端处理边界。
5. 第三方客户端可能不发送 Web 心跳，因此 Emby 播放回调负责刷新转码会话，回调缺失时依靠现有 TTL
   和孤儿目录清理。
6. 客户端 profile 协商依赖已知的本地媒体 codec；实时增量扫描必须在后台仅探测本次新建/变化的
   `LOCAL_FILE` source，不能把待插件处理的 `.strm` source 交给普通 ffprobe，也不能退化为全库扫描。

## 接口合同

### PlaybackInfo

- 接收 Emby 标准 `PlaybackInfoRequest` 的 `MediaSourceId`、`DeviceProfile`、`EnableDirectPlay`、
  `EnableDirectStream`、`EnableTranscoding`、`AllowVideoStreamCopy` 和 `AllowAudioStreamCopy` 等字段。
- `GET` 或空 body `POST`：保持当前响应，不创建转码会话。
- 明确要求服务端转码的 POST：
  - `MediaSourceId` 选择媒体源；query 参数仍可作为兼容回退。
  - 使用 `EnableDirectPlay`、`EnableDirectStream`、`EnableTranscoding`、
    `AllowVideoStreamCopy` 和 `AllowAudioStreamCopy` 选择最低成本档位。
  - `forceTranscode=true` 作为 POST 查询参数时强制选择服务端转码，即使 body 中
    `EnableDirectPlay=true`；GET 不因该参数创建转码会话。
  - 客户端同时允许直放和转码，或未提供顶层 `Enable...` 布尔值时，使用
    `DeviceProfile.DirectPlayProfiles` 匹配媒体源的 `Container`、视频 codec 和音频 codec；直放 profile
    确认不匹配且 `TranscodingProfiles` 声明 HLS 时选择服务端转码；源容器/codec 缺失时不得将未知当作不匹配。
  - 本地 source 返回 `SupportsTranscoding` 与带 HMAC 票据的 `TranscodingUrl`。
  - `.strm` source 保持 `SupportsTranscoding=false`，不返回 `TranscodingUrl`。

### 转码资源

- `GET|HEAD /Videos/{itemId}/master.m3u8`：读取签名的 Emby HLS 清单。
- `GET|HEAD /Videos/{itemId}/transcoding/{sessionId}/{asset}`：读取签名的 init/media asset。
- `/emby` 和大小写兼容路由与其他 Emby 路由一致。
- 清单由服务端重写为同一用户、条目、source、会话和过期时间绑定的签名资产 URL。

### 播放回调

- 转码 `PlaySessionId` 采用 Lux 可识别的前缀；`/Sessions/Playing` 与 `/Progress` 延长相应转码会话 TTL。
- `/Stopped` 立即停止并清理该转码会话，同时保留现有 Emby 进度记录合同。

## 安全与边界

- 每个资源签名绑定 session、asset 和过期时间；服务端额外检查 session 的 user/item/source/plan。
- 资源请求不依赖 Web Cookie，也不把 API token 放入 `TranscodingUrl`。
- 转码输入只允许 `canonical_local_media_path` 返回的本地普通文件；`.strm`、路径穿越和外部目标拒绝。
- 不新增迁移、不修改 Web DTO、不实现服务器字幕转换、DRM 或自适应多码率。

## 预计修改文件

- `src/application/playback/session.rs`：支持 Emby 转码会话标识，并复用 TTL、签名和资源生命周期。
- `src/api/playback.rs`：解析 Emby PlaybackInfo body，创建转码会话，提供 master/asset 路由。
- `src/api/emby.rs`：注册 Emby 转码路由。
- `src/api/legacy.rs`：允许转码路径通过未匹配 Emby video path 防护。
- `tests/playback.rs`：覆盖协商、真实 HLS 资产、ACL、签名、`.strm` 和回调清理。
- `src/application/scanner.rs`、`src/storage/jobs.rs`、`tests/scanning_jobs.rs`：确保增量扫描只探测本任务
  新建/变化的本地 source，并保留 `.strm` 的插件探测边界。
- `docs/API.md`、`docs/COMPATIBILITY.md`：更新公共合同与验证状态。

## 验证命令

窄验证：

```bash
cargo test --locked --test playback
cargo test --locked --lib playback
cargo fmt --all -- --check
```

完成门：

```bash
cargo build --locked
cargo test --locked --all-targets
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
uname -m
```

本机架构结果只记录为 ARM64 环境证据，不代表 FNOS NAS/x86_64 性能或真实第三方客户端已经兼容。
