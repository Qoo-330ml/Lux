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
   Emby 对省略的播放开关按启用处理，因此只提交 `DeviceProfile` 的客户端也能完成标准协商。
   `MaxStreamingBitrate`（顶层或 `DeviceProfile` 内）也参与协商；当本地 source 的已知码率超过该限制且声明了 HLS
   转码 profile 时选择服务端转码，码率未知时不因未知值触发转码。
3. Emby HLS 按播放会话协商分片容器：没有明确容器声明时默认输出 MPEG-TS，
   `TranscodingContainer=ts`、`TranscodingMimeType=video/mp2t`；客户端在
   `DeviceProfile.TranscodingProfiles.Container` 或请求参数中明确声明 `mp4`/`fmp4` 时才输出
   fMP4/CMAF，并返回对应的 `mp4`/`video/mp4`。`TranscodingUrl` 同时返回 Emby 播放器通常使用的
   `DeviceId`、输出 codec、码率、轨道索引、`SegmentContainer`、`MinSegments`、`BreakOnNonKeyFrames`
   和 `TranscodeReasons` 参数；实际转码 offer 的 `DirectStreamUrl` 与 `TranscodingUrl` 指向同一个签名 HLS
   清单，兼容依赖 `DirectStreamUrl` 的客户端；`SupportsDirectPlay`/`SupportsDirectStream` 仍为 `false`，
   不会将该 offer 误报为直放。Lux 的短期 HMAC 参数绑定会话容器和资源，防止 TS/fMP4 混用。
4. `.strm` 是 Direct-only；本任务不扩大它的服务端处理边界。
5. 第三方客户端可能不发送 Web 心跳，因此 Emby 播放回调负责刷新转码会话，回调缺失时依靠现有 TTL
   和孤儿目录清理。
6. 客户端 profile 协商依赖已知的本地媒体 codec；实时增量扫描必须在后台仅探测本次新建/变化的
   `LOCAL_FILE` source，不能把待插件处理的 `.strm` source 交给普通 ffprobe，也不能退化为全库扫描。

## 接口合同

### PlaybackInfo

- 接收 Emby 标准 `PlaybackInfoRequest` 的 `MediaSourceId`、`DeviceProfile`、`EnableDirectPlay`、
  `EnableDirectStream`、`EnableTranscoding`、`AllowVideoStreamCopy` 和 `AllowAudioStreamCopy` 等字段；
  五个播放开关同时兼容放在 POST URL 查询参数中，`DeviceProfile` 通常位于 JSON body。
  同时接收 `MaxStreamingBitrate`，并兼容其放在 POST URL 查询参数中。
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
    如果 `MaxStreamingBitrate` 已知且小于源媒体码率，也选择服务端转码；源码率未知时不因码率限制自动转码。
  - 本地 source 在 HLS `TranscodingProfiles` 可用时返回 `SupportsTranscoding=true`，即使本次仍选择
    Direct Play；实际选择服务端转码时返回带 HMAC 票据的 `TranscodingUrl`，并将
    `SupportsDirectPlay`/`SupportsDirectStream` 置为 `false`，避免客户端绕过该 URL。
  - `.strm` source 保持 `SupportsTranscoding=false`，不返回 `TranscodingUrl`。
- `PlaybackInfo` 响应顶层和每个 `MediaSources[]` 返回完整 `RunTimeTicks`；优先使用选中 source 的探测时长，缺失时回退到媒体项时长。
  Emby HLS 清单必须按完整 `RunTimeTicks` 生成 `#EXT-X-PLAYLIST-TYPE:VOD`、完整分片列表和
  `#EXT-X-ENDLIST`，使第三方客户端立即获得完整时间轴。MPEG-TS 清单直接列出逻辑 `.ts` 分片且不带
  `EXT-X-MAP`；fMP4 清单才为每个逻辑 `segment_N.m4s` 声明对应的 `init_N.mp4`，避免 seek generation
  之间混用 fMP4 时间基准。FFmpeg 仍以增量方式生成分片，尚未生成的签名分片请求由服务端等待。
  Lux Web 的内部 HLS 清单仍保持原有动态追加行为，不复用 Emby 的 VOD 清单。

### 转码资源

- `GET|HEAD /Videos/{itemId}/master.m3u8`：读取签名的 Emby HLS 清单。
- `GET|HEAD /Videos/{itemId}/transcoding/{sessionId}/{asset}`：读取签名的 init/media asset。
- `/emby` 和大小写兼容路由与其他 Emby 路由一致。
- 清单由服务端重写为同一用户、条目、source、会话和过期时间绑定的签名资产 URL。

### 播放回调

- 转码 `PlaySessionId` 采用 Lux 可识别的前缀；`/Sessions/Playing` 与 `/Progress` 延长相应转码会话 TTL。播放回调的 `RunTimeTicks` 不覆盖服务端已探测到的媒体项/媒体源时长；只有服务端没有可用时长时，才使用客户端回调值作为兼容兜底，避免动态 HLS 清单长度成为会话总时长。
- `/Stopped` 立即停止并清理该转码会话，同时保留现有 Emby 进度记录合同。

## 安全与边界

- 每个资源签名绑定 session、asset 和过期时间；服务端额外检查 session 的 user/item/source/plan。
- 资源请求不依赖 Web Cookie，也不把 API token 放入 `TranscodingUrl`。
- 转码 URL 的标准 Emby 参数只描述本次会话的设备和转码选择；设备 ID 从请求 query、标准设备 ID 请求头或
  Emby 鉴权头取得，缺失时使用 `unknown`，不把长期 API token 写入 URL。
- 转码输入只允许 `canonical_local_media_path` 返回的本地普通文件；`.strm`、路径穿越和外部目标拒绝。
- 不新增迁移、不修改 Web DTO、不实现服务器字幕转换、DRM 或自适应多码率。

## Generation / init 绑定修复

Emby fMP4 VOD 不能把所有逻辑分片都映射为动态的单一 `init.mp4`。每个逻辑分片使用
`init_N.mp4`，内部 FFmpeg generation 使用独立的 init 和 segment 文件名；逻辑分片第一次激活后绑定到该
generation，后续 seek 不会把它改映射到另一个 generation。这样 Harbor 先请求恢复位置的 init、随后请求第 0 段时，
两者会分别得到自己的时间基准，不会再组成“中段 init + 第 0 段媒体”的非法 fMP4 组合。TS 会话不创建 init，
也不会进入这条 fMP4 generation 绑定规则，但仍按逻辑 segment 绑定 generation。

仅增加 `-start_at_zero`、`use_editlist=0` 或输出时间戳偏移不能修复该问题：FNOS Jellyfin FFmpeg 7.1.4
在禁用 edit list 后仍会把中段 fragment 的 PTS 归零，因此不采用参数级替代方案。

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
