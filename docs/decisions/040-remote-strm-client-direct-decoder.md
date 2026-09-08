# ADR-040：远程 STRM 浏览器直连与客户端解码 fallback

## 状态

已接受；取代 ADR-039 中“远程字幕通过 Lux signed `rangeUrl`”以及远程 Web 媒体优先使用 Lux Relay 的部分。

## 背景

远程 HTTP(S) `.strm` 的目标媒体位于上游服务，Lux 不应成为网页媒体带宽的中转站。旧 Web 路径把远程 Matroska 的原生播放和字幕
旁路接到 Lux 的 `/range` 或标准 `/Videos` 入口，导致视频、音频和字幕流量回到部署 Lux 的机器，也让上游的 User-Agent、Cookie、
Range 与重定向语义发生变化。

浏览器原生 `<video>` 对容器和 codec 的支持不一致。Lux 已有 `ClientMkvEngine`、`ClientHevcEngine`、Worker、WASM 和 WebCodecs
能力探测，可在浏览器具备相应能力时从远程源直接读取并完成客户端 fallback；但 CORS、Range、稳定的 Content-Range/ETag、MSE、
WASM 性能和 H.264 VideoEncoder 都属于上游或浏览器运行时条件，不能伪造为通用兼容。

## 决定

1. 对 `STRM_URL` 且 `externalUrl` 为 HTTP(S) 的媒体，Lux Web 的媒体 URL 直接使用 `externalUrl`。`proxyUrl`、`rangeUrl`、Lux
   signed Direct URL、Lux HLS 和服务端 ffmpeg 不作为该类 Web 媒体的默认或错误回退路径。
2. 原生 `<video>` 仍是第一路径。原生能力不足时，按现有真实能力探针选择 `ClientHevcEngine` 或 `ClientMkvEngine`；两个引擎的
   输入都是 `externalUrl`，Worker 内的 `fetch`/Range 请求直接发往上游。远程源不支持 CORS/Range 或客户端组合不支持时，显示脱敏
   的失败原因，不重新创建 Lux 播放会话来尝试 Relay。
3. 远程内嵌 SRT/ASS/SSA 的字幕旁路读取器同样直接读取 `externalUrl`，仅在用户选择该字幕后启动。它只读取有限元数据和 Cue 选中的
   Cluster，媒体音视频仍由原生 `<video>` 或客户端 fallback 负责；字幕失败只撤销字幕，不改变媒体 URL、播放状态、播放会话、进度、
   心跳或停止语义。
4. 该直连边界不等于权限绕过：播放会话、ACL、进度、心跳、停止和 UI 状态继续访问 Lux。远程 URL 本身由有播放权限的浏览器获取，
   因此管理员必须自行确保上游 URL 的授权和暴露范围；Lux 不记录完整 URL、令牌或媒体内容。
5. 本地文件、路径型 `.strm`、SMB/FTP 解析、Emby 兼容 API 和第三方客户端的既有代理合同不因本 ADR 改变。

## 后果

- 远程网页播放的媒体带宽不再经过 Lux，且保留上游直连所需的重定向、Cookie、User-Agent 和 Range 语义。
- 远程浏览器解码能力可由现有 WASM/WebCodecs fallback 扩展，但受 CORS、Range、浏览器 codec/MSE 和设备性能限制；不承诺所有
  HEVC、AV1、DTS、PGS 或 4K 组合实时播放。
- 远程字幕支持依赖上游允许脚本 CORS 读取。失败只影响字幕，不应把原本可播放的原生音视频变成 Relay 或服务端转码任务。
- 浏览器开发者工具会看到上游媒体请求，这是直连设计的预期行为；Lux 只能保护控制面，不能对已授予浏览器的远程媒体字节提供 DRM。

## 验证

- Web 单测确认远程原生、HEVC fallback、Matroska fallback 和字幕旁路都收到 `externalUrl`，且远程错误不会改用 `proxyUrl`、
  `rangeUrl` 或 Lux Direct。
- 远程字幕测试确认 Range 请求的 `fetch` 输入为上游 URL，并验证 CORS/206/Content-Range/ETag 失败只报告字幕或客户端能力错误。
- 真实浏览器 network 检查确认视频、音频、字幕媒体请求的 host 为上游而不是 Lux；Lux 请求仅限播放会话和进度等控制接口。
