# ADR-038：远程 STRM 原生播放默认与显式字幕单管线

## 状态

已部分取代；远程字幕的音视频处理和字幕读取边界以 ADR-039 为准。本文保留签名 Range Relay、生命周期和失败隔离约束。

## 日期

2026-09-07

## 背景

当前远程 HTTP(S) STRM 的原生 `<video>` 可以播放音视频，但 Chrome 等浏览器并不保证把 MKV 内嵌字幕暴露为
`video.textTracks`。因此“只消费 native TextTrack”会让 API 已经列出的 SRT/ASS/SSA 轨道一直显示“暂不支持”，如用户实际截图所示。

同时，远程媒体不能在默认播放时交给 JavaScript：Range/CORS/MSE/codec 任一条件失败都会破坏原本可用的 Direct Play。

## 决定

1. 远程 HTTP(S) STRM 默认仍使用原生 `<video>`，优先使用播放计划的 `proxyUrl`，代理失败才沿用签名 Lux Direct URL。未选择字幕时不
   启动 Worker、Range、MSE 或客户端 MKV 引擎。
2. 当用户明确选择远程 Matroska/WebM 的 SRT、ASS 或 SSA 内嵌轨时，播放器保持原生音视频 `<video>`，并通过当前播放会话签名的同源
   `rangeUrl` 启动字幕旁路读取器。读取器只解封装字幕 Cluster，将 Lux cue 交给覆盖层；不把音视频 remux 到 MSE，不请求外部 CDN 的
   JavaScript URL，不预先抽取或生成外挂文件。详细运行时合同以 ADR-039 为准。
3. 选择字幕只启动字幕旁路，不创建新的播放会话，不调用停止接口，不改变媒体源、tier 或 ACL。切换时保留当前播放位置和播放/暂停状态。
4. Range、索引或字幕解析失败时，只清除字幕选择并保持同一播放计划的原生视频；不得进入服务端 HLS，也不得显示
   “播放器引擎失败”。
5. URL 型 HTTP(S) 以外的 STRM 不进入该管线；本地 Matroska 仍使用原有客户端 fallback 和 source-scoped 字幕合同。

## 后果

- 远程音视频继续拥有修改字幕前的原生播放兼容性。
- 浏览器未暴露 native TextTrack 时，用户仍可主动选择远程内嵌文本字幕；播放保持原生音视频，旁路只读取字幕。
- 显式字幕模式依赖同源 Range Relay、有效 Matroska 轨道和 SeekHead/Cues。失败只影响字幕，不会终止原生视频，也不再依赖浏览器 MSE codec 组合。
- 该管线必须继续保持单一逻辑媒体读取器和严格资源上限。未来的 seek/Cues 优化应在该管线内部完成，不得恢复“默认所有远程 MKV 走 JS”。

## 验证

- 无字幕选择时没有 Range 请求，远程 `<video>` 使用原生代理/签名 URL。
- 选择字幕时读取播放会话 `rangeUrl`，不读取外部 URL；SRT/ASS/SSA 轨道进入字幕控制器。
- Range/字幕解析失败时字幕被撤销、同一媒体计划保持不变，播放会话不停止或重建。
