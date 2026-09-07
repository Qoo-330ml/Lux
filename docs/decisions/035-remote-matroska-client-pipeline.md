# ADR-035：远程 STRM 的 Matroska 客户端单管线与内嵌文本字幕

## 状态

已被 ADR-037 取代（ADR-036 的历史中间方案也已被 ADR-037 取代）。本文保留原始客户端单管线的设计和取舍记录，不属于当前远程
STRM 播放合同。

## 日期

2026-09-06

## 背景

当前 Web 播放页把远程 HTTP(S) STRM 交给原生 `<video>` 时，视频播放本身是稳定的；浏览器的媒体管线可能解封装 Matroska，
但页面脚本不一定能从 `HTMLVideoElement.textTracks` 看到内嵌字幕。ffprobe 的字幕列表也不能证明浏览器一定暴露对应轨道。

Lux 不能为远程 STRM 增加服务端拉取、字幕抽取、ffmpeg、HLS 或媒体代理：远程目标可能绑定 User-Agent、Cookie、短期令牌或
单连接语义，服务端接管会破坏现有 Direct Play 边界。与此同时，用户需要在不预先抽取、不落盘、不生成外挂字幕的前提下使用
远程 MKV 内嵌文本字幕，并要求 seek 可用。

## 决定

### 1. 使用浏览器端单一逻辑媒体管线

对于用户显式选择远程内嵌文本字幕的 URL 型 HTTP(S) STRM Matroska/WebM，播放器可使用一个 JavaScript 主线程协调器和 Web Worker：

1. 主线程使用播放会话授权的同源 Range Relay 发起顺序 Range 请求；首段为 `bytes=0-1048575`。
2. Worker 解析 EBML、Segment、Info、Tracks、SeekHead、Cues 和 Cluster。
3. Worker 同时处理音频、视频和文本字幕。音视频输出为浏览器 MSE 可消费的 fMP4，字幕输出为 Lux cue。
4. `<video>` 继续作为 MSE 输出和渲染目标，不再直接负责解封装远程 Matroska。

没有选择远程内嵌字幕时，仍使用原生 `<video>`，不启动该管线。启用后，“单次读取”指一个逻辑媒体读取器，而不是只有一个
HTTP 请求：预取和 seek 可以产生多个串行 Range 请求，但同一代最多一个在途请求，不存在原生视频连接和字幕专用连接并行读取。

### 2. 必须具备可定位索引

Relay 上游响应必须是脚本可读取的 `206 + Content-Range`，且 Segment 中存在有效 SeekHead/Cues。Cues 必须能够定位所选视频轨的
关键 Cluster。禁止为了建立索引而顺序扫描整部文件；缺少索引、Range 被忽略、范围/总长度不一致、资源发生变化或解析超限时，
当前客户端直接显示“不支持”，不回退到原生播放、Lux HLS、媒体代理或 302/Redia 字幕接口。

首版只处理 HTTP(S) URL 型 STRM。SMB、FTP、路径型远程目标以及依赖远端 Cookie 或自定义 User-Agent 的资源不进入该管线。
Relay 只接受当前播放会话的签名票据、单区间 Range，最大 32 MiB；不落盘、不缓存整部媒体、不调用 ffmpeg。

### 3. 首版 codec 和字幕边界

首版尝试直通 H.264、HEVC、VP9、AV1 视频，以及 AAC、AC-3、E-AC-3、Opus 音频。完整视频/音频组合必须通过实际的
`MediaSource.isTypeSupported` 检查；浏览器不支持的组合直接失败。保留现有 HEVC WASM 解码并编码为 H.264 的客户端 fallback，
但不为 VP9/AV1 增加软件转码。

文本字幕只支持 `S_TEXT/UTF8`、`S_TEXT/ASS`、`S_TEXT/SSA`。字幕轨道使用 TrackUID 形成稳定选择键；缺少 UID 时使用源内
TrackNumber fallback。字幕时间来自 Block timestamp 和 BlockDuration，字幕切换不重新创建播放会话。

ASS/SSA 首版只实现安全基础样式：颜色、粗体、斜体、对齐、margin、位置和必要转义。动画、move、karaoke、clip、旋转、
字体嵌入、drawing、PGS/SUP、DRM、服务器烧录和完整样式不属于本决定。

### 4. 安全和资源上限

远程 EBML、codec private、Cluster 和字幕文本均视为不可信输入。实现必须验证 VINT、元素边界、Segment 偏移、嵌套深度、
Track/Cue/cue 数量、文本长度、内存和取消状态；错误不能泄露完整 URL、令牌、Cookie、路径或媒体内容。字幕使用 React 文本节点
和 allowlist 样式，不使用 `innerHTML`。

默认上限为：元数据/Cues 元素 8 MiB、64 条媒体轨、16 条字幕轨、100,000 个 CuePoint、字幕缓存总计 16 MiB、单 cue 文本
64 KiB、EBML 嵌套 16 层、单次媒体 Range 32 MiB。超过上限直接终止当前客户端播放。

## 备选方案

### 继续只依赖原生 `<video>`

拒绝作为远程字幕承诺。原生媒体管线内部可能拥有字幕数据，但页面脚本无法要求它把不可见的 Matroska cue 暴露出来。

### Lux 服务端抽取远程字幕

拒绝。会改变远程资源的访问主体和连接语义，扩大 SSRF、带宽、隐私和资源治理风险，并违反 STRM Direct Play 边界。

### 读取整部远程媒体后再建立字幕索引

拒绝。无法支持长视频和及时 seek，也会造成无界内存、带宽和等待时间；Cues 是首版的必要索引合同。

### 通过额外字幕 URL 或 302/Redia 接口提供字幕

拒绝。用户要求使用同一远程媒体内嵌字幕，这些接口会变成第二条媒体/字幕读取路径，也会引入不稳定的外部合同。

## 后果

- 远程 MKV 内嵌文本字幕可以在浏览器端实时解封装并显示，不产生外挂文件或服务端媒体流量。
- 远程视频播放继续由原生 `<video>` 保证；远程字幕模式的兼容性取决于 Relay、Range、SeekHead/Cues、MSE 和实际 codec 组合，
  条件不满足时只禁用远程字幕，不得让原生视频播放失败。
- MSE、Range reader、Matroska demuxer、字幕 cue 和播放器生命周期必须作为一个 generation 管理，seek/切源时清理旧请求和 cue。
- ADR-032 的本地字幕按需抽取、native TextTrack 优先和不处理 PGS/SUP 等决定继续有效；其远程实验默认关闭和失败回退部分被本 ADR 取代。

## 验证

- Web 单测覆盖 EBML 分块、SeekHead/Cues、BlockGroup/BlockDuration、字幕样式、Range 取消、MSE codec 能力和 generation 清理。
- 固定媒体夹具覆盖 H.264+AAC+SRT、HEVC+E-AC-3+ASS、VP9+Opus+SSA、AV1+AAC+ASS。
- Chrome、Firefox、Safari 真实浏览器验证网络请求边界、字幕切换、seek、停止和错误回退；只记录实际成功的 codec 组合。
