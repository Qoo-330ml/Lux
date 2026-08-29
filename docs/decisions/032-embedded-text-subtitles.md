# ADR-032：内嵌文本字幕的浏览器优先与远程 STRM 隔离

## 状态

已接受（LUX-224）。

## 日期

2026-08-29

## 背景

Lux 已经通过媒体探测记录内嵌字幕流，也已经有受 ACL 保护的外挂字幕读取端点和 Web Worker 文本字幕解析器。用户希望
本地 MKV 中的 SRT/ASS/SSA 可以选择，同时远程 `.strm` 继续由播放器或外部代理直连，媒体字节不经过 Lux。

这两个目标不能简单合并为“服务端统一抽取”：远程 `.strm` 可能指向要求特定 User-Agent、Cookie、短期令牌或单连接的网盘资源。
Lux 拉取它会改变资源的访问主体和连接语义，也会把本来 Direct Play 的媒体流量引入服务端。另一方面，浏览器的 `<video>` 元素
是否能看到 Matroska 内嵌字幕，取决于浏览器实际使用的媒体管线；ffprobe 列出字幕流，不代表页面脚本一定能从
`HTMLVideoElement.textTracks` 读到同一条轨。

ArtPlayer 官方实现提供了可核验的边界，但没有证明普通网页播放器会自动解封装远程 MKV：

- 固定快照为 ArtPlayer 仓库 commit
  [`02d1ded7b8601b8cc654e33d066d996968c7bdc0`](https://github.com/zhw2590582/ArtPlayer/tree/02d1ded7b8601b8cc654e33d066d996968c7bdc0)。
- 核心 [`packages/artplayer/src/subtitle.js`](https://github.com/zhw2590582/ArtPlayer/blob/02d1ded7b8601b8cc654e33d066d996968c7bdc0/packages/artplayer/src/subtitle.js)
  接收 `subtitle.url`，对 SRT/ASS/VTT 进行 `fetch`，转为 WebVTT Blob，再挂载一个新的 `<track>`；其 `textTracks` 读取的是
  这个页面创建的字幕轨。
- [`artplayer-plugin-multiple-subtitles`](https://github.com/zhw2590582/ArtPlayer/tree/02d1ded7b8601b8cc654e33d066d996968c7bdc0/packages/artplayer-plugin-multiple-subtitles)
  也是读取多个字幕 URL 后合并成 WebVTT。
- [`artplayer-plugin-jassub`](https://github.com/zhw2590582/ArtPlayer/tree/02d1ded7b8601b8cc654e33d066d996968c7bdc0/packages/artplayer-plugin-jassub)
  是额外的浏览器侧 ASS/SSA Canvas 渲染器，仍需要单独取得 ASS 数据。
- [`artplayer-proxy-mediabunny`](https://github.com/zhw2590582/ArtPlayer/tree/02d1ded7b8601b8cc654e33d066d996968c7bdc0/packages/artplayer-proxy-mediabunny)
  把媒体管线代理到 Canvas，并在当前快照中选择视频/音频轨；它不是 Lux 可以据此承诺普通远程 MKV 内嵌字幕的依据。

## 决策

### 1. 本地文本字幕使用按需抽取 fallback

对于本地、已授权且已索引的 SRT/ASS/SSA 内嵌轨：

1. Web 播放器首先检查当前视频实例真实暴露的 in-band `TextTrack`。
2. 若没有可用 native track，则通过 source-scoped Lux 字幕合同请求所选流。
3. Rust 只对本地媒体做有界、按需、无转码抽取；结果交给现有 Web Worker 文本解析和覆盖层。

抽取不写回媒体、不烧录、不建立永久缓存、不处理 PGS/SUP。首版 ASS/SSA 只保证对白文本和时间，不保证完整样式、定位和动画。

### 2. 远程 `.strm` 保持 Direct Play 隔离

远程 URL/path `.strm` 不由 Lux 读取媒体字节，不由 Lux 启动 ffprobe/ffmpeg，不进入 Lux HLS 或媒体代理，也不新增 302/Redia
字幕专用接口。视频仍按现有规则由播放器或已配置的外部代理直接请求目标资源。

远程内嵌字幕只有两种可能：

- 当前浏览器的实际媒体管线把文本轨暴露为 `video.textTracks`，由 LuxPlayer 只切换当前 track；或
- 后续实验性客户端管线在显式开关下满足 CORS、Range、鉴权、读取上限、单连接和生命周期取消条件，并复用当前媒体读取。

第二种默认关闭。实验失败、资源是一次性 User-Agent/令牌绑定、CORS/Range 不满足或解析器不支持时，视频必须继续按原有 Direct
Play 播放，字幕显示不可用原因即可。不得为了字幕隐式改成服务端 HLS、代理媒体字节、重新建立第二条远程连接或调用 302/Redia
字幕接口。

### 3. 字幕是媒体源外的展示状态

字幕选择绑定当前 `itemId`、`mediaSourceId` 和播放器生命周期，但不进入播放会话创建或计划决策。选择、关闭、偏移和清理不得
改变媒体 URL、请求头、User-Agent、tier、HLS/fallback、ACL、进度、心跳、停止或页面离开语义。所有 source/engine/destroy
路径都必须释放旧 track、cue、Worker 和 AbortController。

## 备选方案

### 由 Lux 统一代理并抽取所有远程 `.strm`

拒绝。它会让服务器承担远程媒体流量，破坏 Direct Play 和外部资源的 UA/令牌/单连接约束，并扩大 SSRF、超时、带宽和隐私风险。

### 让浏览器脚本直接从普通 `<video>` 读取所有 MKV 内嵌轨

拒绝作为无条件承诺。只有浏览器运行时实际暴露的 `TextTrack` 才可使用；标准 `textTracks` API 不能把页面不可见的 Matroska
demux 数据凭空变成可读 cue。

### 直接引入 ArtPlayer 或 JASSUB

拒绝作为 Lux 运行时依赖。ArtPlayer 可以作为公开实现参考，JASSUB 也可作为未来完整 ASS 样式能力的候选，但两者都不能解决
远程资源访问授权问题，且会把第三方生命周期、DOM、依赖和播放模型引入 Lux。

### 新增 302/Redia 字幕专用接口

拒绝。该接口会把字幕能力绑定到外部代理的非稳定合同，且与 Lux Web 的媒体源/ACL/生命周期模型重复；本阶段由 native track 和
Lux 自有 source-scoped 字幕合同覆盖。

## 后果

- 本地媒体的内嵌文本字幕有可测试、可控、有上限的 fallback；不需要用户先把字幕另存为外挂文件。
- 远程 `.strm` 的视频直连边界保持不变，但远程内嵌字幕只能按浏览器实际能力提供，不承诺所有浏览器和所有网盘资源。
- PGS/SUP、完整 ASS 样式、服务器烧录、HLS 字幕组和远程服务端抽取需要另立 ADR，不得通过本阶段的 fallback 偷渡。
- 兼容性记录必须分别说明视频字节来源和字幕数据来源；ffprobe 轨道枚举只能作为提示，不能作为 Web 可用性证据。

## 验证

- ArtPlayer 机制按固定 commit、官方文档和源码路径核验；Lux 不复制核心字幕代码。
- Rust 测试覆盖 source ACL、本地抽取、格式/大小/取消边界和远程 `.strm` 零读取。
- Web 测试覆盖 native `TextTrack` 探测、source/engine/destroy 清理、字幕选择不创建播放会话和实验失败回退。
- 阶段门使用固定无个人数据夹具检查视频请求与字幕请求边界，并记录浏览器、平台、夹具哈希及 `uname -m`。
